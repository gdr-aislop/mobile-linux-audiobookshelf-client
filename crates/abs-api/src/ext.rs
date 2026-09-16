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

#[derive(Debug, Deserialize)]
struct LibraryItemsResponseBody {
    results: Vec<RawLibraryItem>,
}

#[derive(Debug, Deserialize)]
struct RawLibraryItem {
    id: Option<String>,
    #[serde(rename = "addedAt")]
    added_at: Option<i64>,
    media: Option<RawMedia>,
}

#[derive(Debug, Deserialize)]
struct RawMedia {
    duration: Option<f64>,
    metadata: Option<RawMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    title: Option<String>,
    #[serde(rename = "authorName")]
    author_name: Option<String>,
    #[serde(rename = "narratorName")]
    narrator_name: Option<String>,
    description: Option<String>,
}

/// The vendored spec declares `BearerAuth` as a security scheme on its authenticated operations,
/// but `progenitor` doesn't generate a per-call header parameter for it — confirmed by grepping
/// the generated `client.rs` for "Authorization"/"bearer" (no matches at all). So every
/// authenticated call (everything except `login`) needs a client built with this constructor
/// rather than the plain `Client::new`, which sends no auth at all and gets `401`s back (caught
/// live against `https://audiobooks.dev/audiobookshelf`, not just in theory).
#[derive(Debug, thiserror::Error)]
#[error("access token contains characters that aren't valid in an HTTP header")]
pub struct InvalidBearerToken;

impl Client {
    /// Builds a client that attaches `Authorization: Bearer <token>` to every request it sends.
    /// `token` is normally `LoginResult::access_token` fresh from `login`, or an already-persisted
    /// account's stored token.
    pub fn with_bearer_token(baseurl: &str, token: &str) -> Result<Self, InvalidBearerToken> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| InvalidBearerToken)?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("a reqwest::Client with only headers/timeouts set should never fail to build");

        Ok(Self::new_with_client(baseurl, http))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryItemSummary {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub description: Option<String>,
    pub duration_seconds: f64,
    /// Milliseconds since the Unix epoch, as the server sends it — left as a raw `i64` here (not
    /// `chrono::DateTime`) so this crate doesn't need to pick a time-handling dependency just for
    /// one field; callers (`abs-core`) already depend on `chrono` for the storage layer's models
    /// and can convert.
    pub added_at_ms: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryItemsError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

impl Client {
    /// Fetch a library's items with their media metadata (title, author, narrator, description,
    /// duration). Hand-written rather than using the generated `get_library_items`: that call's
    /// response type (`GetLibraryItemsResponse::results: Vec<LibraryItemBase>`) has no `media`
    /// field at all — a real gap in the vendored spec's schema for this endpoint, confirmed by
    /// checking `LibraryItemBase`'s generated fields and by hitting the real server directly (see
    /// `third_party/audiobookshelf-openapi/README.md`'s "Known gaps"): it always sends
    /// `media.metadata.{title,authorName,narratorName,description}` and `media.duration`, the
    /// generated type just silently drops them on deserialize. Items missing an id, title, or the
    /// whole `media`/`metadata` object are skipped rather than erroring the whole page — a
    /// malformed entry shouldn't take down every other item in the library.
    pub async fn get_library_items_with_media(
        &self,
        library_id: &str,
    ) -> Result<Vec<LibraryItemSummary>, LibraryItemsError> {
        let response = self
            .client()
            .get(format!("{}/api/libraries/{library_id}/items", self.baseurl()))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!(
                "GET /api/libraries/{library_id}/items returned HTTP {}",
                response.status()
            )));
        }

        let body: LibraryItemsResponseBody = response.json().await?;

        Ok(body
            .results
            .into_iter()
            .filter_map(|item| {
                let id = item.id?;
                let media = item.media?;
                let metadata = media.metadata?;
                let title = metadata.title?;
                Some(LibraryItemSummary {
                    id,
                    title,
                    author: metadata.author_name,
                    narrator: metadata.narrator_name,
                    description: metadata.description,
                    duration_seconds: media.duration.unwrap_or(0.0),
                    added_at_ms: item.added_at.unwrap_or(0),
                })
            })
            .collect())
    }

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
    async fn with_bearer_token_sends_the_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .and(header("authorization", "Bearer secret-token-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "libraries": [] })))
            .mount(&server)
            .await;

        let client = Client::with_bearer_token(&server.uri(), "secret-token-123").unwrap();
        let response = client.get_libraries().await.unwrap();
        assert!(response.into_inner().libraries.is_empty());
    }

    #[tokio::test]
    async fn a_plain_client_sends_no_authorization_header_and_gets_rejected() {
        // Regression test: caught live against the real demo server, where `Client::new` (no
        // auth) got a 401 back from an authenticated endpoint. wiremock rejects the request here
        // for the same reason — no Authorization header was ever attached.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries"))
            .and(header("authorization", "Bearer secret-token-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "libraries": [] })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let result = client.get_libraries().await;
        assert!(result.is_err(), "a request with no Authorization header should not match the mock");
    }

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
    async fn get_library_items_with_media_parses_title_author_and_duration() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{
                    "id": "item-1",
                    "addedAt": 1_700_000_000_000i64,
                    "media": {
                        "duration": 3600.5,
                        "metadata": {
                            "title": "Project Hail Mary",
                            "authorName": "Andy Weir",
                            "narratorName": "Ray Porter",
                            "description": "A lone astronaut...",
                        }
                    }
                }]
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let items = client.get_library_items_with_media("lib-1").await.unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "item-1");
        assert_eq!(items[0].title, "Project Hail Mary");
        assert_eq!(items[0].author.as_deref(), Some("Andy Weir"));
        assert_eq!(items[0].narrator.as_deref(), Some("Ray Porter"));
        assert_eq!(items[0].duration_seconds, 3600.5);
        assert_eq!(items[0].added_at_ms, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn get_library_items_with_media_skips_entries_missing_media_or_title() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "id": "no-media" },
                    { "id": "no-title", "media": { "metadata": {} } },
                    {
                        "id": "item-1",
                        "media": { "metadata": { "title": "Valid Item" } }
                    },
                ]
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let items = client.get_library_items_with_media("lib-1").await.unwrap();

        assert_eq!(items.len(), 1, "only the well-formed item should survive");
        assert_eq!(items[0].id, "item-1");
        assert_eq!(items[0].duration_seconds, 0.0, "missing duration defaults to 0.0");
    }

    #[tokio::test]
    async fn get_library_items_with_media_propagates_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/libraries/lib-1/items"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.get_library_items_with_media("lib-1").await.unwrap_err();
        assert!(matches!(err, LibraryItemsError::UnexpectedResponse(_)));
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
