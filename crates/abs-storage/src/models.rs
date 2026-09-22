//! Plain data types shared across `repo/*` modules. These are storage-layer models (they mirror
//! table rows), not the API's wire types (`abs-api`'s generated `types::*`) or UI view-models —
//! `abs-core` is responsible for mapping between the three.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Server {
    pub id: String,
    pub url: String,
    pub custom_headers_json: String,
    pub disable_ssl_verify: bool,
    pub client_cert_path: Option<String>,
    /// The PKCS#12 bundle's export password (see `repo::servers`). Plaintext, like the account
    /// tokens this database already holds.
    pub client_cert_password: Option<String>,
    pub local_network_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Account {
    pub id: String,
    pub server_id: String,
    pub username: String,
    pub token: String,
    /// The JWT-auth refresh token (server v2.26.0+): long-lived, rotated on every refresh, and
    /// what keeps a session alive past the access token's expiry. `None` for legacy servers
    /// (permanent tokens that never expire).
    pub refresh_token: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Library {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub media_type: String,
    pub icon: Option<String>,
    pub display_order: i64,
    pub synced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Item {
    pub id: String,
    pub server_id: String,
    pub library_id: String,
    pub title: String,
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub description: Option<String>,
    pub cover_cache_path: Option<String>,
    pub duration_seconds: f64,
    pub added_at: DateTime<Utc>,
    pub synced_at: DateTime<Utc>,
    pub series_name: Option<String>,
    /// The `genres` column, JSON-encoded (SQLite has no array type) — same "encode a compound
    /// value into one TEXT column" convention `servers.custom_headers_json` already uses. Decode
    /// via [`Item::genres`] rather than parsing this directly.
    pub genres_json: String,
}

impl Item {
    /// Decodes [`Item::genres_json`]. Malformed JSON (which should never happen — this app is the
    /// only writer) degrades to an empty list rather than failing the whole render.
    pub fn genres(&self) -> Vec<String> {
        serde_json::from_str(&self.genres_json).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Chapter {
    pub server_id: String,
    pub item_id: String,
    pub chapter_index: i64,
    pub title: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Progress {
    pub account_id: String,
    pub server_id: String,
    pub item_id: String,
    pub current_time_seconds: f64,
    pub is_finished: bool,
    pub updated_at: DateTime<Utc>,
}

/// Cached per-item track metadata — mirrors `abs_core::streaming::StreamTrack`, persisted so
/// downloads (and, later, offline-availability checks) don't need a network call to know what a
/// book's tracks are. `size_bytes` is the server-reported file size, when known — what size
/// estimates are computed from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Track {
    pub server_id: String,
    pub item_id: String,
    pub ino: String,
    pub track_index: i64,
    pub duration_seconds: f64,
    pub offset_seconds: f64,
    pub size_bytes: Option<i64>,
}

/// A track download's lifecycle. `Downloading` and `Failed` both retain whatever
/// `bytes_downloaded` was reached, so a `Downloading` row left behind by an ungraceful app exit is
/// just as resumable as a `Failed` one — the status only reflects "what happened last", not
/// whether resuming is possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Complete,
    Failed,
}

impl DownloadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DownloadStatus::Pending => "pending",
            DownloadStatus::Downloading => "downloading",
            DownloadStatus::Complete => "complete",
            DownloadStatus::Failed => "failed",
        }
    }
}

impl std::str::FromStr for DownloadStatus {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(DownloadStatus::Pending),
            "downloading" => Ok(DownloadStatus::Downloading),
            "complete" => Ok(DownloadStatus::Complete),
            "failed" => Ok(DownloadStatus::Failed),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadTrack {
    pub server_id: String,
    pub item_id: String,
    pub ino: String,
    pub file_path: String,
    pub expected_size_bytes: Option<i64>,
    pub bytes_downloaded: i64,
    pub status: DownloadStatus,
    pub error_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}
