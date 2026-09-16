//! Hand-written extensions to the generated [`Client`], for endpoints confirmed missing from the
//! vendored OpenAPI spec entirely — see `third_party/audiobookshelf-openapi/README.md`'s "Known
//! gaps". These live here, as a plain `impl Client` using the same [`Client::client`] /
//! [`Client::baseurl`] accessors the generated code itself is built on, so every caller depends
//! on exactly one client type regardless of whether a given method happens to be generated or
//! hand-written. If upstream ever documents one of these endpoints, the generated method takes
//! over and the hand-written one here is deleted.

use serde::{Deserialize, Serialize};

use crate::Client;

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Deserialize)]
struct LoginResponseBody {
    user: LoginUser,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginUser {
    id: String,
    username: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoginResult {
    pub user_id: String,
    pub username: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

impl Client {
    /// Log in with a username and password, returning the tokens needed for subsequent
    /// authenticated requests. Not in the vendored spec at all (no auth endpoints are documented)
    /// — hand-written against the real route confirmed in the server source (`server/Auth.js`):
    /// `POST {baseurl}/login` with a JSON `{username, password}` body, passport-local under the
    /// hood. The `x-return-tokens: true` header is what makes it return the access/refresh tokens
    /// in the JSON body instead of only setting a cookie — required for any non-browser client,
    /// this one included.
    pub async fn login(&self, username: &str, password: &str) -> Result<LoginResult, LoginError> {
        let response = self
            .client()
            .post(format!("{}/login", self.baseurl()))
            .header("x-return-tokens", "true")
            .json(&LoginRequest { username, password })
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(LoginError::InvalidCredentials);
        }
        if !response.status().is_success() {
            return Err(LoginError::UnexpectedResponse(format!(
                "login returned HTTP {}",
                response.status()
            )));
        }

        let body: LoginResponseBody = response.json().await?;
        let access_token = body.user.access_token.ok_or_else(|| {
            LoginError::UnexpectedResponse(
                "login succeeded but response carried no accessToken (was x-return-tokens sent?)"
                    .into(),
            )
        })?;

        Ok(LoginResult {
            user_id: body.user.id,
            username: body.user.username,
            access_token,
            refresh_token: body.user.refresh_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn successful_login_returns_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .and(header("x-return-tokens", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": "user-1",
                    "username": "jane",
                    "accessToken": "abc123",
                    "refreshToken": "refresh456",
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let result = client.login("jane", "hunter2").await.unwrap();

        assert_eq!(result.user_id, "user-1");
        assert_eq!(result.username, "jane");
        assert_eq!(result.access_token, "abc123");
        assert_eq!(result.refresh_token.as_deref(), Some("refresh456"));
    }

    #[tokio::test]
    async fn wrong_password_surfaces_as_invalid_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.login("jane", "wrong").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidCredentials));
    }

    #[tokio::test]
    async fn server_error_surfaces_as_unexpected_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.login("jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::UnexpectedResponse(_)));
    }

    #[tokio::test]
    async fn missing_access_token_in_response_is_an_unexpected_response_error() {
        // Simulates hitting a server that ignored x-return-tokens (e.g. an older version) —
        // this must not panic or silently produce an empty token.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": { "id": "user-1", "username": "jane" }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.login("jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::UnexpectedResponse(_)));
    }
}
