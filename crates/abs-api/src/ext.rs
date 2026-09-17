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
    #[error("this session's login has expired — sign in again")]
    SessionExpired,
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
            LoginError::InvalidCredentials | LoginError::SessionExpired | LoginError::UnexpectedResponse(_) => return None,
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
    /// account's stored token. Uses a 15s connect/request timeout — long enough to tolerate a slow
    /// mobile connection, short enough that a call never hangs indefinitely when the server or
    /// network is simply gone (offline, airplane mode, etc.).
    pub fn with_bearer_token(baseurl: &str, token: &str) -> Result<Self, InvalidBearerToken> {
        Self::with_bearer_token_and_timeout(baseurl, token, std::time::Duration::from_secs(15))
    }

    /// Same as [`Client::with_bearer_token`], with a caller-chosen timeout instead of the default
    /// 15s. For calls on a critical path (e.g. resolving a playable URL) the default is
    /// appropriate; for best-effort background work (e.g. reconciling progress against the
    /// server before falling back to what's already stored locally) a much shorter timeout keeps
    /// a bad connection from being felt as a hang.
    pub fn with_bearer_token_and_timeout(
        baseurl: &str,
        token: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, InvalidBearerToken> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| InvalidBearerToken)?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(timeout)
            .timeout(timeout)
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

#[derive(Debug, Deserialize)]
struct ItemDetailResponseBody {
    media: Option<RawItemMedia>,
}

#[derive(Debug, Deserialize)]
struct RawItemMedia {
    #[serde(rename = "audioFiles", default)]
    audio_files: Vec<RawAudioFile>,
    #[serde(default)]
    chapters: Vec<RawChapter>,
}

#[derive(Debug, Deserialize)]
struct RawAudioFile {
    ino: Option<String>,
    duration: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawChapter {
    title: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioFileRef {
    pub ino: String,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChapterRef {
    pub title: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ItemPlaybackInfo {
    pub audio_files: Vec<AudioFileRef>,
    pub chapters: Vec<ChapterRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoverBytes {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

#[derive(Debug, Serialize)]
struct UpdateProgressRequest {
    #[serde(rename = "currentTime")]
    current_time: f64,
    duration: f64,
    #[serde(rename = "isFinished")]
    is_finished: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerProgress {
    pub library_item_id: String,
    pub current_time_seconds: f64,
    pub duration_seconds: f64,
    pub is_finished: bool,
    /// Milliseconds since the Unix epoch — the server's own field is a plain JS timestamp, not an
    /// RFC3339 string, so callers reconciling against `abs_storage`'s `DateTime<Utc>` need to
    /// convert it themselves (`chrono::DateTime::from_timestamp_millis`).
    pub last_update_ms: i64,
}

#[derive(Debug, Deserialize)]
struct RawMediaProgress {
    #[serde(rename = "libraryItemId")]
    library_item_id: Option<String>,
    #[serde(rename = "currentTime")]
    current_time: Option<f64>,
    duration: Option<f64>,
    #[serde(rename = "isFinished")]
    is_finished: Option<bool>,
    #[serde(rename = "lastUpdate")]
    last_update: Option<i64>,
}

impl RawMediaProgress {
    fn into_server_progress(self) -> Option<ServerProgress> {
        Some(ServerProgress {
            library_item_id: self.library_item_id?,
            current_time_seconds: self.current_time.unwrap_or(0.0),
            duration_seconds: self.duration.unwrap_or(0.0),
            is_finished: self.is_finished.unwrap_or(false),
            last_update_ms: self.last_update.unwrap_or(0),
        })
    }
}

#[derive(Debug, Deserialize)]
struct MeResponseBody {
    #[serde(rename = "mediaProgress", default)]
    media_progress: Vec<RawMediaProgress>,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryItemsError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

/// A dedicated error type for [`Client::get_item_file_response`] rather than reusing
/// [`LibraryItemsError`] — a download pipeline needs to tell a 4xx (bad request/auth/not found,
/// never worth retrying) apart from a 5xx or a network error (worth retrying with backoff), which
/// `LibraryItemsError::UnexpectedResponse`'s plain `String` throws away.
#[derive(Debug, thiserror::Error)]
pub enum TrackFileError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("server returned HTTP {0}")]
    Status(reqwest::StatusCode),
}

impl TrackFileError {
    /// A 4xx means the request itself is wrong (bad token, item/track no longer exists) — retrying
    /// the exact same request will never succeed, unlike a `5xx`/network blip.
    pub fn is_retryable(&self) -> bool {
        match self {
            TrackFileError::Network(_) => true,
            TrackFileError::Status(status) => !status.is_client_error(),
        }
    }
}

/// A track's audio file response, ready to be streamed to disk. See
/// [`Client::get_item_file_response`] for why this exposes the live [`reqwest::Response`] instead
/// of pre-collecting its body.
#[derive(Debug)]
pub struct TrackFileResponse {
    pub resumed: bool,
    pub total_size: Option<u64>,
    pub content_type: Option<String>,
    pub response: reqwest::Response,
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

    /// Fetch the audio file(s) backing an item, for resolving a playable URL. Hand-written: the
    /// vendored spec has **zero** `/api/items/*` coverage at all (see
    /// `third_party/audiobookshelf-openapi/README.md`'s "Known gaps") — there is no generated
    /// method to call here. Confirmed live against `https://audiobooks.dev/audiobookshelf` that
    /// `GET /api/items/:id` returns `media.audioFiles[].{ino, duration}`, and that
    /// `GET /api/items/:id/file/:ino?token=<access_token>` streams the actual audio bytes — so
    /// this only needs to parse the `ino`/`duration` pair per file; the actual streaming URL is
    /// assembled by the caller (`abs_core::streaming::resolve_stream_target`), not here.
    pub async fn get_item_playback_info(&self, item_id: &str) -> Result<ItemPlaybackInfo, LibraryItemsError> {
        let response = self.client().get(format!("{}/api/items/{item_id}", self.baseurl())).send().await?;

        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!(
                "GET /api/items/{item_id} returned HTTP {}",
                response.status()
            )));
        }

        let body: ItemDetailResponseBody = response.json().await?;
        let (audio_files, chapters) = match body.media {
            Some(media) => (media.audio_files, media.chapters),
            None => (Vec::new(), Vec::new()),
        };
        let audio_files = audio_files
            .into_iter()
            .filter_map(|f| {
                Some(AudioFileRef { ino: f.ino?, duration_seconds: f.duration.unwrap_or(0.0) })
            })
            .collect();
        // A malformed chapter entry (missing title/start/end) is skipped rather than failing the
        // whole call, same tolerance as audio files above — one bad chapter shouldn't take down
        // playback or the chapters sheet for the rest of the book.
        let chapters = chapters
            .into_iter()
            .filter_map(|c| Some(ChapterRef { title: c.title?, start_seconds: c.start?, end_seconds: c.end? }))
            .collect();

        Ok(ItemPlaybackInfo { audio_files, chapters })
    }

    /// Fetch an item's cover image. Hand-written, same `/api/items/*` gap as
    /// `get_item_playback_info`. Confirmed live: `GET /api/items/:id/cover` with the bearer-token
    /// client (no `?token=` query param needed, unlike `/file/:ino`) returns `200` and the raw
    /// image bytes; the content-type varies by library (`image/webp` for the one checked live,
    /// but Audiobookshelf may return jpeg/png depending on the source cover file), so callers must
    /// not assume a fixed extension.
    pub async fn get_item_cover(&self, item_id: &str) -> Result<CoverBytes, LibraryItemsError> {
        let response = self.client().get(format!("{}/api/items/{item_id}/cover", self.baseurl())).send().await?;

        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!(
                "GET /api/items/{item_id}/cover returned HTTP {}",
                response.status()
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        let bytes = response.bytes().await?.to_vec();
        Ok(CoverBytes { bytes, content_type })
    }

    /// Fetch a track's audio file, optionally resuming with an HTTP `Range` request — the raw
    /// per-track byte source a download pipeline streams to disk (as opposed to `StreamTarget`'s
    /// URL, which is handed to GStreamer for playback and never fetched as bytes in Rust). Same
    /// `/api/items/*` gap as the other hand-written extensions above; confirmed live: the file
    /// endpoint sends `accept-ranges: bytes` and honors `Range: bytes=N-` with a `206` and a
    /// matching `Content-Range`, exactly like a normal HTTP file server.
    ///
    /// Returns the live `reqwest::Response` unconsumed (not `.bytes()`-collected) so the caller can
    /// stream it in chunks — collecting a whole audiobook track into memory before writing it out
    /// would defeat the point of a resumable, progress-reporting download. `resumed` distinguishes
    /// a server that actually honored the `Range` request (`206`) from one that ignored it and sent
    /// the full body from byte 0 (`200`) — callers must check this rather than assuming a `Some`
    /// `range_start` was respected, since blindly appending a full-body response onto existing
    /// partial bytes would corrupt the file. `total_size` is always the *whole file's* size (parsed
    /// from `Content-Range`'s `.../total` suffix when resumed, since a `206`'s `Content-Length` is
    /// only the remaining-bytes count, not the file's total size) — never the size of just this
    /// response's body, so callers can report accurate progress against the true track size
    /// regardless of where a resume started from.
    pub async fn get_item_file_response(&self, item_id: &str, ino: &str, range_start: Option<u64>) -> Result<TrackFileResponse, TrackFileError> {
        let mut request = self.client().get(format!("{}/api/items/{item_id}/file/{ino}", self.baseurl()));
        if let Some(start) = range_start {
            request = request.header(reqwest::header::RANGE, format!("bytes={start}-"));
        }
        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(TrackFileError::Status(response.status()));
        }

        let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let total_size = if resumed {
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.rsplit('/').next())
                .and_then(|s| s.parse::<u64>().ok())
        } else {
            response.content_length()
        };
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);

        Ok(TrackFileResponse { resumed, total_size, content_type, response })
    }

    /// Push local playback progress up to the server, so it shows up in the official apps and
    /// survives a fresh install. Hand-written: `/api/me/*` has zero coverage in the vendored spec
    /// (see `third_party/audiobookshelf-openapi/README.md`'s "Known gaps"). Confirmed live against
    /// `https://audiobooks.dev/audiobookshelf`: `PATCH /api/me/progress/:libraryItemId` with a
    /// JSON body of `{currentTime, duration, isFinished}` returns `200 OK` and the change shows up
    /// in a subsequent `GET /api/me`'s `mediaProgress`; a nonexistent item id returns `404`. The
    /// server computes its own `progress` fraction — passing one had no visible effect — so this
    /// only sends the fields that actually matter.
    pub async fn update_media_progress(
        &self,
        item_id: &str,
        current_time_seconds: f64,
        duration_seconds: f64,
        is_finished: bool,
    ) -> Result<(), LibraryItemsError> {
        let response = self
            .client()
            .patch(format!("{}/api/me/progress/{item_id}", self.baseurl()))
            .json(&UpdateProgressRequest { current_time: current_time_seconds, duration: duration_seconds, is_finished })
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!(
                "PATCH /api/me/progress/{item_id} returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    /// Fetch this item's progress as the server currently has it, for reconciling against what's
    /// stored locally before resuming playback — so opening a book reflects progress made on
    /// another device or the official apps, not just this client's own last local write.
    /// Hand-written, same `/api/me/*` gap as `update_media_progress`. Confirmed live: `GET
    /// /api/me/progress/:libraryItemId` returns `200` with the progress object when one exists,
    /// and `404` when it doesn't — which is a normal, expected "no progress yet" outcome here, not
    /// an error.
    pub async fn get_media_progress(&self, item_id: &str) -> Result<Option<ServerProgress>, LibraryItemsError> {
        let response = self.client().get(format!("{}/api/me/progress/{item_id}", self.baseurl())).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!(
                "GET /api/me/progress/{item_id} returned HTTP {}",
                response.status()
            )));
        }

        let raw: RawMediaProgress = response.json().await?;
        Ok(raw.into_server_progress())
    }

    /// Fetch every item this account has progress on, in one call — for reconciling Home's
    /// "Continue Listening" shelf against the server without one request per item. Hand-written:
    /// `GET /api/me` isn't in the vendored spec either, and returns the full user object with a
    /// `mediaProgress` array alongside fields this client has no use for (permissions, accessible
    /// libraries, etc.), which are simply ignored here.
    pub async fn get_all_media_progress(&self) -> Result<Vec<ServerProgress>, LibraryItemsError> {
        let response = self.client().get(format!("{}/api/me", self.baseurl())).send().await?;

        if !response.status().is_success() {
            return Err(LibraryItemsError::UnexpectedResponse(format!("GET /api/me returned HTTP {}", response.status())));
        }

        let body: MeResponseBody = response.json().await?;
        Ok(body.media_progress.into_iter().filter_map(RawMediaProgress::into_server_progress).collect())
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

    /// Exchange a refresh token for a fresh token pair. Hand-written (no auth endpoints are in the
    /// vendored spec — same as `login`), against the JWT auth system's documented shape (server
    /// v2.26.0+, github.com/advplyr/audiobookshelf discussion #4460): `POST {baseurl}/auth/refresh`
    /// with the refresh token in the `x-refresh-token` header (mobile clients don't get cookies),
    /// responding with the same body shape as `/login`. The refresh token **rotates on every
    /// use** — the new pair from the response must replace the old one in storage, and a 401 here
    /// means the server no longer knows this session at all (expired, revoked, or the server lost
    /// its session store), so the only way back in is signing in again.
    pub async fn refresh(&self, refresh_token: &str) -> Result<LoginResult, LoginError> {
        let response = self
            .client()
            .post(format!("{}/auth/refresh", self.baseurl()))
            .header("x-return-tokens", "true")
            .header("x-refresh-token", refresh_token)
            .send()
            .await
            .map_err(classify_transport_error)?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(LoginError::SessionExpired);
        }
        if !response.status().is_success() {
            return Err(LoginError::UnexpectedResponse(format!(
                "token refresh returned HTTP {}",
                response.status()
            )));
        }

        let body: LoginResponseBody = response.json().await.map_err(classify_transport_error)?;
        let access_token = body.user.access_token.ok_or_else(|| {
            LoginError::UnexpectedResponse(
                "token refresh succeeded but response carried no accessToken (was x-return-tokens sent?)".into(),
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
    use wiremock::matchers::{body_partial_json, header, method, path};
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

    /// A closed port refuses a connection immediately — it doesn't exercise the *timeout* at all,
    /// just normal error handling. This proves the timeout itself actually cuts off a request
    /// that's genuinely stuck (TCP connects fine, the server just never answers — closer to what
    /// "connectivity silently drops mid-request" looks like than an immediate refusal), so a real
    /// network failure can never hang the caller indefinitely regardless of what stage it's stuck
    /// at. Uses a 200ms timeout (not the 15s/5s a real caller would use) purely to keep this test
    /// fast; the mechanism being tested is the same.
    #[tokio::test]
    async fn with_bearer_token_and_timeout_does_not_hang_on_a_server_that_never_responds() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Accept the connection and hold it open forever without writing a response.
            let _ = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(60));
        });

        let client = Client::with_bearer_token_and_timeout(&format!("http://{addr}"), "token", std::time::Duration::from_millis(200)).unwrap();

        let started = std::time::Instant::now();
        let result = client.get_media_progress("item-1").await;
        assert!(result.is_err(), "a request to a server that never responds must time out, not succeed");
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "the timeout should cut this off in ~200ms, not hang");
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

    /// End-to-end against the real public demo server: logs in, finds a real library and item,
    /// then confirms `get_item_playback_info` actually returns a usable audio file — this is what
    /// caught, live, that the real server needs the login the same way every other call does
    /// (`with_bearer_token`, not a plain `Client::new`). `#[ignore]`d; run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn get_item_playback_info_against_the_live_demo_server() {
        const DEMO_SERVER_URL: &str = "https://audiobooks.dev/audiobookshelf";
        let login = Client::new(DEMO_SERVER_URL).login("demo", "demo").await.unwrap();
        let api = Client::with_bearer_token(DEMO_SERVER_URL, &login.access_token).unwrap();

        let libraries = api.get_libraries().await.unwrap().into_inner().libraries;
        let library_id = libraries[0].id.clone().expect("the demo server's first library should have an id");
        let items = api.get_library_items_with_media(&library_id.to_string()).await.unwrap();
        let item = items.first().expect("the demo server's first library should have at least one item");

        let info = api.get_item_playback_info(&item.id).await.unwrap();
        let audio_file = info.audio_files.first().expect("a real item should have at least one audio file");
        assert!(!audio_file.ino.is_empty());
        assert!(audio_file.duration_seconds > 0.0);
        if let Some(chapter) = info.chapters.first() {
            assert!(!chapter.title.is_empty());
            assert!(chapter.end_seconds > chapter.start_seconds);
        }
    }

    /// End-to-end against the real public demo server: logs in, updates progress for a real item,
    /// then confirms via `GET /api/me` that it actually landed server-side — `PATCH
    /// /api/me/progress/:id` returning `200 OK` isn't itself proof the server persisted anything.
    /// `#[ignore]`d; run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn update_media_progress_against_the_live_demo_server() {
        const DEMO_SERVER_URL: &str = "https://audiobooks.dev/audiobookshelf";
        let login = Client::new(DEMO_SERVER_URL).login("demo", "demo").await.unwrap();
        let api = Client::with_bearer_token(DEMO_SERVER_URL, &login.access_token).unwrap();

        let libraries = api.get_libraries().await.unwrap().into_inner().libraries;
        let library_id = libraries[0].id.clone().expect("the demo server's first library should have an id");
        let items = api.get_library_items_with_media(&library_id.to_string()).await.unwrap();
        let item = items.first().expect("the demo server's first library should have at least one item");

        api.update_media_progress(&item.id, 77.0, item.duration_seconds, false).await.unwrap();

        let me: serde_json::Value =
            api.client().get(format!("{DEMO_SERVER_URL}/api/me")).send().await.unwrap().json().await.unwrap();
        let synced = me["mediaProgress"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["libraryItemId"] == item.id)
            .expect("the item just updated should appear in mediaProgress");
        assert_eq!(synced["currentTime"].as_f64().unwrap(), 77.0);
    }

    /// End-to-end against the real public demo server: pushes a known progress value, then reads
    /// it back through both `get_media_progress` (the single-item endpoint) and
    /// `get_all_media_progress` (the bulk `/api/me` endpoint) and checks both agree with what was
    /// written. `#[ignore]`d; run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn get_media_progress_against_the_live_demo_server() {
        const DEMO_SERVER_URL: &str = "https://audiobooks.dev/audiobookshelf";
        let login = Client::new(DEMO_SERVER_URL).login("demo", "demo").await.unwrap();
        let api = Client::with_bearer_token(DEMO_SERVER_URL, &login.access_token).unwrap();

        let libraries = api.get_libraries().await.unwrap().into_inner().libraries;
        let library_id = libraries[0].id.clone().expect("the demo server's first library should have an id");
        let items = api.get_library_items_with_media(&library_id.to_string()).await.unwrap();
        let item = items.first().expect("the demo server's first library should have at least one item");

        api.update_media_progress(&item.id, 99.0, item.duration_seconds, false).await.unwrap();

        let single = api.get_media_progress(&item.id).await.unwrap().expect("progress was just written");
        assert_eq!(single.current_time_seconds, 99.0);

        let all = api.get_all_media_progress().await.unwrap();
        let same_item = all.iter().find(|p| p.library_item_id == item.id).expect("should also appear in the bulk list");
        assert_eq!(same_item.current_time_seconds, 99.0);
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
    async fn get_item_playback_info_parses_audio_files() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [
                        { "ino": "111", "duration": 1800.5 },
                        { "ino": "222", "duration": 1200.0 },
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();

        assert_eq!(info.audio_files.len(), 2);
        assert_eq!(info.audio_files[0].ino, "111");
        assert_eq!(info.audio_files[0].duration_seconds, 1800.5);
        assert_eq!(info.audio_files[1].ino, "222");
    }

    #[tokio::test]
    async fn get_item_playback_info_with_no_audio_files_is_an_empty_vec_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [] }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();
        assert!(info.audio_files.is_empty());
    }

    #[tokio::test]
    async fn get_item_playback_info_skips_audio_files_missing_ino() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [
                        { "duration": 1800.5 },
                        { "ino": "222", "duration": 1200.0 },
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();

        assert_eq!(info.audio_files.len(), 1);
        assert_eq!(info.audio_files[0].ino, "222");
    }

    #[tokio::test]
    async fn get_item_playback_info_propagates_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.get_item_playback_info("item-1").await.unwrap_err();
        assert!(matches!(err, LibraryItemsError::UnexpectedResponse(_)));
    }

    #[tokio::test]
    async fn get_item_playback_info_parses_chapters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [{ "ino": "1", "duration": 3600.0 }],
                    "chapters": [
                        { "id": 0, "start": 0.0, "end": 100.0, "title": "Introduction" },
                        { "id": 1, "start": 100.0, "end": 3600.0, "title": "Chapter 1" },
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();

        assert_eq!(info.chapters.len(), 2);
        assert_eq!(info.chapters[0].title, "Introduction");
        assert_eq!(info.chapters[0].start_seconds, 0.0);
        assert_eq!(info.chapters[0].end_seconds, 100.0);
        assert_eq!(info.chapters[1].title, "Chapter 1");
    }

    #[tokio::test]
    async fn get_item_playback_info_skips_malformed_chapters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [{ "ino": "1", "duration": 3600.0 }],
                    "chapters": [
                        { "id": 0, "start": 0.0, "title": "Missing end" },
                        { "id": 1, "start": 10.0, "end": 20.0, "title": "Valid" },
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();

        assert_eq!(info.chapters.len(), 1, "a chapter missing a required field should be skipped, not fail the call");
        assert_eq!(info.chapters[0].title, "Valid");
    }

    #[tokio::test]
    async fn get_item_playback_info_with_no_chapters_is_an_empty_vec_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": 3600.0 }] }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let info = client.get_item_playback_info("item-1").await.unwrap();
        assert!(info.chapters.is_empty());
    }

    #[tokio::test]
    async fn get_item_cover_returns_bytes_and_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/webp").set_body_bytes(vec![1, 2, 3, 4]))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let cover = client.get_item_cover("item-1").await.unwrap();
        assert_eq!(cover.bytes, vec![1, 2, 3, 4]);
        assert_eq!(cover.content_type, "image/webp");
    }

    #[tokio::test]
    async fn get_item_cover_defaults_to_jpeg_when_no_content_type_is_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1, 2, 3]))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let cover = client.get_item_cover("item-1").await.unwrap();
        assert_eq!(cover.content_type, "image/jpeg");
    }

    #[tokio::test]
    async fn get_item_cover_propagates_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/cover")).respond_with(ResponseTemplate::new(404)).mount(&server).await;

        let client = Client::new(&server.uri());
        let err = client.get_item_cover("item-1").await.unwrap_err();
        assert!(matches!(err, LibraryItemsError::UnexpectedResponse(_)));
    }

    #[tokio::test]
    async fn get_item_file_response_without_range_returns_the_full_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "4").insert_header("Content-Type", "audio/mpeg").set_body_bytes(vec![1, 2, 3, 4]))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let file = client.get_item_file_response("item-1", "ino-1", None).await.unwrap();
        assert!(!file.resumed, "a plain 200 response was not a resumed range");
        assert_eq!(file.total_size, Some(4));
        assert_eq!(file.content_type.as_deref(), Some("audio/mpeg"));
        let bytes = file.response.bytes().await.unwrap();
        assert_eq!(bytes.as_ref(), &[1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn get_item_file_response_sends_a_range_header_and_recognizes_206() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .and(header("Range", "bytes=100-"))
            .respond_with(ResponseTemplate::new(206).insert_header("Content-Range", "bytes 100-103/104").set_body_bytes(vec![9, 9, 9, 9]))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let file = client.get_item_file_response("item-1", "ino-1", Some(100)).await.unwrap();
        assert!(file.resumed, "a 206 response means the server actually honored the Range request");
        assert_eq!(file.total_size, Some(104), "total_size must be the whole file's size, not just this response's 4-byte body");
    }

    #[tokio::test]
    async fn get_item_file_response_detects_a_server_that_ignores_range() {
        let server = MockServer::start().await;
        // Some servers reply 200 with the *full* body even when a Range header was sent — the
        // caller must be able to tell this apart from a real 206 resume, or it would corrupt a
        // partially-downloaded file by blindly appending onto it.
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .and(header("Range", "bytes=100-"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0; 104]))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let file = client.get_item_file_response("item-1", "ino-1", Some(100)).await.unwrap();
        assert!(!file.resumed, "a 200 in response to a Range request must not be treated as resumed");
    }

    #[tokio::test]
    async fn get_item_file_response_surfaces_the_status_on_a_client_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/file/ino-1")).respond_with(ResponseTemplate::new(404)).mount(&server).await;

        let client = Client::new(&server.uri());
        let err = client.get_item_file_response("item-1", "ino-1", None).await.unwrap_err();
        assert!(!err.is_retryable(), "a 404 should never be retried");
        assert!(matches!(err, TrackFileError::Status(status) if status == reqwest::StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn get_item_file_response_a_server_error_is_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/file/ino-1")).respond_with(ResponseTemplate::new(503)).mount(&server).await;

        let client = Client::new(&server.uri());
        let err = client.get_item_file_response("item-1", "ino-1", None).await.unwrap_err();
        assert!(err.is_retryable(), "a 5xx should be retried");
    }

    #[tokio::test]
    async fn update_media_progress_sends_the_expected_body() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/me/progress/item-1"))
            .and(body_partial_json(serde_json::json!({
                "currentTime": 42.5,
                "duration": 200.0,
                "isFinished": false,
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        client.update_media_progress("item-1", 42.5, 200.0, false).await.unwrap();
    }

    #[tokio::test]
    async fn update_media_progress_propagates_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.update_media_progress("item-1", 42.5, 200.0, false).await.unwrap_err();
        assert!(matches!(err, LibraryItemsError::UnexpectedResponse(_)));
    }

    #[tokio::test]
    async fn get_media_progress_parses_an_existing_record() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraryItemId": "item-1",
                "currentTime": 123.5,
                "duration": 3600.0,
                "isFinished": false,
                "lastUpdate": 1_700_000_000_000i64,
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let progress = client.get_media_progress("item-1").await.unwrap().expect("a progress record exists");
        assert_eq!(progress.library_item_id, "item-1");
        assert_eq!(progress.current_time_seconds, 123.5);
        assert_eq!(progress.last_update_ms, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn get_media_progress_with_no_record_is_none_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        assert!(client.get_media_progress("item-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_media_progress_propagates_other_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let err = client.get_media_progress("item-1").await.unwrap_err();
        assert!(matches!(err, LibraryItemsError::UnexpectedResponse(_)));
    }

    #[tokio::test]
    async fn get_all_media_progress_parses_every_record() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "mediaProgress": [
                    { "libraryItemId": "item-1", "currentTime": 10.0, "duration": 100.0, "isFinished": false, "lastUpdate": 1 },
                    { "libraryItemId": "item-2", "currentTime": 20.0, "duration": 200.0, "isFinished": true, "lastUpdate": 2 },
                ]
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        let all = client.get_all_media_progress().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].library_item_id, "item-1");
        assert_eq!(all[1].library_item_id, "item-2");
        assert!(all[1].is_finished);
    }

    #[tokio::test]
    async fn get_all_media_progress_with_no_progress_is_an_empty_vec() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "mediaProgress": [] })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri());
        assert!(client.get_all_media_progress().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_all_media_progress_propagates_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/me")).respond_with(ResponseTemplate::new(500)).mount(&server).await;

        let client = Client::new(&server.uri());
        let err = client.get_all_media_progress().await.unwrap_err();
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
