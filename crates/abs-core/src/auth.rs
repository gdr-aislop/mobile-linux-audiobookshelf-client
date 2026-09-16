//! Login is not in the vendored OpenAPI spec at all (see `docs/api/README.md`'s "Handling
//! endpoints the spec doesn't cover"), so this hand-writes the one call against the real route
//! confirmed in the server source (`server/Auth.js`): `POST /login` with a JSON `{username,
//! password}` body, passport-local under the hood. The `x-return-tokens: true` header is what
//! makes it return the access/refresh tokens in the JSON body instead of only setting a cookie —
//! required for any non-browser client, this one included.

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

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

/// Log in with a username and password against `base_url`, returning the tokens needed for
/// subsequent authenticated requests. `base_url` should not have a trailing slash.
pub async fn login(
    http: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<LoginResult> {
    let response = http
        .post(format!("{base_url}/login"))
        .header("x-return-tokens", "true")
        .json(&LoginRequest { username, password })
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CoreError::AuthFailed("invalid username or password".into()));
    }
    if !response.status().is_success() {
        return Err(CoreError::UnexpectedResponse(format!(
            "login returned HTTP {}",
            response.status()
        )));
    }

    let body: LoginResponseBody = response.json().await?;
    let access_token = body.user.access_token.ok_or_else(|| {
        CoreError::UnexpectedResponse(
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

        let http = reqwest::Client::new();
        let result = login(&http, &server.uri(), "jane", "hunter2").await.unwrap();

        assert_eq!(result.user_id, "user-1");
        assert_eq!(result.username, "jane");
        assert_eq!(result.access_token, "abc123");
        assert_eq!(result.refresh_token.as_deref(), Some("refresh456"));
    }

    #[tokio::test]
    async fn wrong_password_surfaces_as_auth_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = login(&http, &server.uri(), "jane", "wrong").await.unwrap_err();
        assert!(matches!(err, CoreError::AuthFailed(_)));
    }

    #[tokio::test]
    async fn server_error_surfaces_as_unexpected_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = login(&http, &server.uri(), "jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, CoreError::UnexpectedResponse(_)));
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

        let http = reqwest::Client::new();
        let err = login(&http, &server.uri(), "jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, CoreError::UnexpectedResponse(_)));
    }
}
