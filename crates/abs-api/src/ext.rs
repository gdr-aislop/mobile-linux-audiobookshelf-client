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
    #[error("couldn't verify this server's TLS certificate: {0}")]
    Tls(reqwest::Error),
    #[error("couldn't connect to this server: {0}")]
    Connect(reqwest::Error),
    #[error("timed out waiting for this server to respond: {0}")]
    Timeout(reqwest::Error),
    #[error("network error: {0}")]
    Network(reqwest::Error),
    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

impl LoginError {
    /// The full underlying error text (this error plus its `source()` chain), for an optional
    /// "Show details" disclosure in the UI — never the primary message, since raw HTTP/TLS
    /// library text isn't meant for a general audience, but worth having for a self-hosted user
    /// debugging an unusual TLS/proxy setup.
    pub fn details(&self) -> Option<String> {
        let err: &(dyn std::error::Error + 'static) = match self {
            LoginError::Tls(e) | LoginError::Connect(e) | LoginError::Timeout(e) | LoginError::Network(e) => e,
            LoginError::InvalidCredentials | LoginError::UnexpectedResponse(_) => return None,
        };
        let mut parts = vec![err.to_string()];
        let mut current = err.source();
        while let Some(source) = current {
            parts.push(source.to_string());
            current = source.source();
        }
        Some(parts.join(" → "))
    }
}

/// Turns a raw transport error into one of `LoginError`'s categorized variants.
fn classify_transport_error(err: reqwest::Error) -> LoginError {
    if err.is_timeout() {
        return LoginError::Timeout(err);
    }
    if is_tls_error(&err) {
        return LoginError::Tls(err);
    }
    if err.is_connect() {
        return LoginError::Connect(err);
    }
    LoginError::Network(err)
}

/// Walks the error's `source()` chain looking for a TLS/certificate cause. String-matching on
/// `Display` text (rather than downcasting to a concrete native-tls/rustls/openssl error type) is
/// deliberate: reqwest is built here with its default TLS backend (native-tls/OpenSSL on Linux —
/// confirmed via Cargo.toml/Cargo.lock, no `rustls-tls` feature enabled), and matching on wording
/// keeps this working across whatever backend or version actually surfaces the error, at the cost
/// of being best-effort — a false negative just falls through to `Connect`/`Network`, never wrong
/// in the dangerous direction (it never *hides* a real TLS error as a credentials problem).
fn is_tls_error(err: &(dyn std::error::Error + 'static)) -> bool {
    const MARKERS: [&str; 6] =
        ["certificate", "self signed", "self-signed", "ssl", "tls", "handshake"];
    let text = err.to_string().to_lowercase();
    if MARKERS.iter().any(|m| text.contains(m)) {
        return true;
    }
    err.source().is_some_and(is_tls_error)
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
            .await
            .map_err(classify_transport_error)?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(LoginError::InvalidCredentials);
        }
        if !response.status().is_success() {
            return Err(LoginError::UnexpectedResponse(format!(
                "login returned HTTP {}",
                response.status()
            )));
        }

        let body: LoginResponseBody = response.json().await.map_err(classify_transport_error)?;
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
    async fn connection_refused_surfaces_as_connect_error_with_details() {
        // Nothing listens on this address, so this fails immediately without any real network —
        // fast and hermetic, unlike the TLS test below.
        let client = Client::new("http://127.0.0.1:1");
        let err = client.login("jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::Connect(_)), "got {err:?}");
        assert!(err.details().is_some(), "a Connect error should carry raw details");
    }

    /// Confirms `classify_transport_error`'s TLS detection against a real handshake failure, not
    /// just the marker-string logic in isolation — badssl.com's self-signed subdomain exists
    /// specifically for tests like this one. `#[ignore]`d so `cargo test --workspace` stays
    /// hermetic; run explicitly with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn untrusted_certificate_surfaces_as_tls_error_with_details() {
        let client = Client::new("https://self-signed.badssl.com");
        let err = client.login("jane", "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::Tls(_)), "got {err:?}");
        let details = err.details().expect("a Tls error should carry raw details");
        assert!(
            details.to_lowercase().contains("cert") || details.to_lowercase().contains("ssl"),
            "expected certificate-related detail text, got: {details}"
        );
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
