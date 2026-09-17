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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Download {
    pub server_id: String,
    pub item_id: String,
    pub chapter_index: i64,
    pub file_path: String,
    pub file_size_bytes: i64,
    pub downloaded_at: DateTime<Utc>,
}

/// A download is "fully" downloaded when every chapter of the item has a row; "partially" when
/// at least one chapter does — matching the offline-mode toggle's "fully or partially downloaded"
/// filter from `docs/design/ui-spec.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCompleteness {
    None,
    Partial,
    Full,
}
