//! Fetches and locally caches an item's cover image. Best-effort, same posture as
//! `progress_sync`'s reconciliation: a cover is cosmetic, never required for playback, so a
//! failure here is logged by the caller and treated as "no cover" rather than a playback error.
//! Callers run this detached from anything time-critical (the player spawns it as a background
//! task after playback starts; Home/Library fetch covers in a post-render task), so the timeout
//! only bounds how long a slow server keeps the cover itself pending — it can never delay
//! playback or any already-rendered UI. A screen's burst of covers goes through
//! [`fetch_and_cache_covers`], which shares one HTTP client (one connection pool) across the
//! whole batch, so the burst amortizes a single DNS+TCP+TLS handshake instead of paying one
//! per cover — and gets HTTP/2 multiplexing over the pooled connection for free.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use sqlx::SqlitePool;

use abs_storage::AppPaths;

/// Generous by design: a batch shares one pooled connection, but on networks where even the
/// first handshake can take several seconds — and where a server may then still be slow to
/// answer — a per-request ceiling keeps a slow-but-working server from starving every cover
/// into an error. Not a hang guard: nothing waits on these fetches.
const COVER_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// How many covers of one batch may be in flight at once over the shared connection pool —
/// enough overlap to keep the connection busy without hammering a small self-hosted server
/// with dozens of simultaneous image requests.
const COVER_FETCH_CONCURRENCY: usize = 6;

/// Returns the local path to the item's cached cover, fetching and writing it first if it isn't
/// already cached. Returns `None` on any failure (offline, 404, disk error, ...) — callers treat
/// "no cover" as a normal outcome, not something to surface to the user.
///
/// The single-item entry point, for callers fetching exactly one cover (the player). Screen
/// bursts should use [`fetch_and_cache_covers`], which reuses one client across the batch.
pub async fn fetch_and_cache_cover(
    paths: &AppPaths,
    pool: &SqlitePool,
    connection: &crate::connection::ConnectionTarget,
    access_token: &str,
    server_id: &str,
    item_id: &str,
) -> Option<PathBuf> {
    let api = connection.api_client_with_timeout(access_token, COVER_FETCH_TIMEOUT).ok()?;
    fetch_and_cache_cover_with(&api, paths, pool, server_id, item_id).await
}

/// Fetches covers for a whole batch of items over one shared HTTP client — one mint, one
/// connection pool, so h2 multiplexing and keep-alive pooling actually get a chance instead
/// of every cover paying a full DNS+TCP+TLS handshake before its first byte. Items already
/// cached issue no HTTP at all; a failure on one item (404, offline, disk error) is logged
/// and never aborts its siblings. Best-effort throughout: no result is returned, callers
/// re-read local storage to pick up whatever landed.
pub async fn fetch_and_cache_covers(
    paths: &AppPaths,
    pool: &SqlitePool,
    connection: &crate::connection::ConnectionTarget,
    access_token: &str,
    server_id: &str,
    item_ids: Vec<String>,
) {
    let api = match connection.api_client_with_timeout(access_token, COVER_FETCH_TIMEOUT) {
        Ok(api) => api,
        Err(err) => {
            tracing::warn!(%err, count = item_ids.len(), "couldn't build a client for the cover batch");
            return;
        }
    };

    futures::stream::iter(item_ids.into_iter().map(|item_id| {
        // Cheap `Client` clone (Arc'd internals) — every future shares the same pool.
        let api = api.clone();
        async move { fetch_and_cache_cover_with(&api, paths, pool, server_id, &item_id).await; }
    }))
    .buffer_unordered(COVER_FETCH_CONCURRENCY)
    .collect::<()>()
    .await;
}

/// The per-item work behind both entry points — everything after client minting, so a batch
/// runs it concurrently over clones of one shared client. The cache check comes first: the
/// common case on repeat visits (cover already on disk) never touches the client at all.
async fn fetch_and_cache_cover_with(
    api: &abs_api::Client,
    paths: &AppPaths,
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
) -> Option<PathBuf> {
    if let Some(cached) = cached_cover_path(pool, server_id, item_id).await {
        return Some(cached);
    }

    let cover = match api.get_item_cover(item_id).await {
        Ok(cover) => {
            // A server (or a proxy in front of it) can answer 200 with a body that isn't an
            // image at all. Such bytes must never reach the cache: a recorded
            // `cover_cache_path` is trusted on file existence alone (see `cached_cover_path`),
            // so garbage would permanently evict this item's real cover everywhere it's
            // displayed — the player replacing a good cached cover with a white placeholder
            // included. Same decoder feature set as the app's `CoverImage` fallback, so
            // "valid enough to cache" and "decodable for display" can't drift apart.
            if let Err(err) = image::load_from_memory(&cover.bytes) {
                tracing::warn!(item_id, content_type = %cover.content_type, %err, "server returned a non-image cover body; ignoring it");
                return None;
            }
            cover
        }
        Err(err) => {
            // `details` classifies the failure (timeout / tls / connect / ...) and walks the
            // full source chain — reqwest's Display alone is just "error sending request for
            // url (...)", identical for a timeout, a DNS failure and a certificate error.
            tracing::warn!(details = err.details(), item_id, "couldn't fetch cover art");
            return None;
        }
    };

    let extension = crate::media_type::extension_for(&cover.content_type, "jpg");
    let dir = paths.covers_dir().join(server_id);
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(%err, item_id, "couldn't create the covers cache directory");
        return None;
    }
    let path = paths.cover_cache_path(server_id, item_id, extension);
    if let Err(err) = tokio::fs::write(&path, &cover.bytes).await {
        tracing::warn!(%err, item_id, "couldn't write the cached cover image");
        return None;
    }
    if let Err(err) = abs_storage::repo::items::set_cover_cache_path(pool, server_id, item_id, Some(&path.to_string_lossy())).await {
        tracing::warn!(%err, item_id, "couldn't record the cached cover path");
    }
    Some(path)
}

/// The item's locally cached cover path, if one is recorded **and** the file it points to still
/// actually exists on disk — the cache directory is evictable (`$XDG_CACHE_HOME`), so the
/// recorded path can go stale without this client's own doing. This is the one canonical read
/// of "what cover do we already have": the player seeds its snapshot from it (so the cover is
/// there from the first frame, with no network involved), Home/Library render it directly, and
/// `fetch_and_cache_cover_with` uses it as its cache hit.
pub async fn cached_cover_path(pool: &SqlitePool, server_id: &str, item_id: &str) -> Option<PathBuf> {
    let item = abs_storage::repo::items::get(pool, server_id, item_id).await.ok()?;
    let existing = Path::new(item.cover_cache_path.as_deref()?).to_path_buf();
    tokio::fs::metadata(&existing).await.ok()?;
    Some(existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionTarget;
    use abs_storage::repo::{accounts, items, libraries, servers};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool_with_synced_items(item_ids: &[&str]) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);

        let server_id = servers::add(&pool, "http://example.invalid").await.unwrap();
        accounts::add(&pool, &server_id, "jane", "token", None).await.unwrap();
        libraries::upsert(
            &pool,
            libraries::UpsertLibrary { id: "lib-1", server_id: &server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 },
        )
        .await
        .unwrap();
        for item_id in item_ids {
            items::upsert(
                &pool,
                items::UpsertItem {
                    id: item_id,
                    server_id: &server_id,
                    library_id: "lib-1",
                    title: "Test Item",
                    author: None,
                    narrator: None,
                    description: None,
                    duration_seconds: 0.0,
                    added_at: chrono::Utc::now(),
                },
            )
            .await
            .unwrap();
        }
        (pool, server_id)
    }

    fn test_paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
        (tmp, paths)
    }

    /// Real, decodable 1x1 images — since downloaded cover bodies are validated with the
    /// `image` crate before being cached, fake byte vectors would now (correctly) be rejected.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63,
        0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82,
    ];
    const WEBP_1X1: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x3c, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x20, 0x30, 0x00, 0x00, 0x00, 0xd0,
        0x01, 0x00, 0x9d, 0x01, 0x2a, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x34, 0x25, 0xa0, 0x02, 0x74, 0xba, 0x01, 0xf8, 0x00, 0x03,
        0xb0, 0x00, 0xfe, 0xf0, 0xc4, 0x0b, 0xff, 0x20, 0xb9, 0x61, 0x75, 0xc8, 0xd7, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff,
        0xf8, 0xf2, 0x00, 0x00, 0x00,
    ];

    #[tokio::test]
    async fn fetch_and_cache_cover_writes_the_file_with_the_right_extension() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/webp").set_body_bytes(WEBP_1X1.to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        let cached = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await;
        let cached = cached.expect("fetch should succeed");

        assert_eq!(cached.extension().unwrap(), "webp");
        assert_eq!(tokio::fs::read(&cached).await.unwrap(), WEBP_1X1);
        let item = abs_storage::repo::items::get(&pool, &server_id, "item-1").await.unwrap();
        assert_eq!(item.cover_cache_path.as_deref(), Some(cached.to_str().unwrap()));
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_ignores_a_non_image_body() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "image/png")
                    .set_body_bytes(b"<html>definitely not an image</html>".to_vec()),
            )
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        let result = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await;
        assert!(result.is_none(), "a non-image body must be treated as a failed fetch");
        let item = abs_storage::repo::items::get(&pool, &server_id, "item-1").await.unwrap();
        assert!(item.cover_cache_path.is_none(), "a non-image body must never be recorded as the cached cover");
        assert!(!paths.covers_dir().exists(), "a rejected body must not be written to disk either");
    }

    #[tokio::test]
    async fn cached_cover_path_trusts_a_recorded_path_only_while_the_file_exists() {
        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        assert!(cached_cover_path(&pool, &server_id, "item-1").await.is_none(), "nothing recorded yet");

        let real = paths.cover_cache_path(&server_id, "item-1", "png");
        tokio::fs::create_dir_all(real.parent().unwrap()).await.unwrap();
        tokio::fs::write(&real, PNG_1X1).await.unwrap();
        abs_storage::repo::items::set_cover_cache_path(&pool, &server_id, "item-1", Some(&real.to_string_lossy())).await.unwrap();
        assert_eq!(cached_cover_path(&pool, &server_id, "item-1").await.as_deref(), Some(real.as_path()));

        tokio::fs::remove_file(&real).await.unwrap();
        assert!(cached_cover_path(&pool, &server_id, "item-1").await.is_none(), "an evicted cache file must not be trusted");
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_reuses_an_existing_cache_hit() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(PNG_1X1.to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        let first = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await.unwrap();
        let second = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await.unwrap();
        assert_eq!(first, second);

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a second call should be a cache hit, not a second HTTP request");
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_returns_none_on_a_server_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/cover")).respond_with(ResponseTemplate::new(404)).mount(&mock_server).await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        let result = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_refetches_if_the_cached_file_was_deleted() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(PNG_1X1.to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1"]).await;

        let cached = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await.unwrap();
        tokio::fs::remove_file(&cached).await.unwrap();

        let refetched = fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await;
        assert!(refetched.is_some(), "a deleted cache file must not be trusted as a hit");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn fetch_and_cache_covers_fetches_every_uncached_item_and_records_the_paths() {
        let mock_server = MockServer::start().await;
        for (item_id, content_type, body) in [("item-1", "image/png", PNG_1X1), ("item-2", "image/webp", WEBP_1X1)] {
            Mock::given(method("GET"))
                .and(path(format!("/api/items/{item_id}/cover")))
                .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", content_type).set_body_bytes(body.to_vec()))
                .mount(&mock_server)
                .await;
        }

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1", "item-2"]).await;

        fetch_and_cache_covers(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, vec!["item-1".into(), "item-2".into()]).await;

        for (item_id, _content_type, body) in [("item-1", "image/png", PNG_1X1), ("item-2", "image/webp", WEBP_1X1)] {
            let item = abs_storage::repo::items::get(&pool, &server_id, item_id).await.unwrap();
            let cached = PathBuf::from(item.cover_cache_path.expect("every batched item's cover should be recorded"));
            assert_eq!(tokio::fs::read(&cached).await.unwrap(), body);
        }
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn fetch_and_cache_covers_keeps_going_when_one_item_fails() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/cover")).respond_with(ResponseTemplate::new(404)).mount(&mock_server).await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-2/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(PNG_1X1.to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1", "item-2"]).await;

        fetch_and_cache_covers(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, vec!["item-1".into(), "item-2".into()]).await;

        let failed = abs_storage::repo::items::get(&pool, &server_id, "item-1").await.unwrap();
        assert!(failed.cover_cache_path.is_none(), "a failed item must not record a cover path");
        let sibling = abs_storage::repo::items::get(&pool, &server_id, "item-2").await.unwrap();
        assert!(sibling.cover_cache_path.is_some(), "one item's failure must not abort its siblings");
    }

    #[tokio::test]
    async fn fetch_and_cache_covers_skips_http_for_already_cached_items() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(PNG_1X1.to_vec()))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-2/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(PNG_1X1.to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_items(&["item-1", "item-2"]).await;

        // Pre-cache item-1 through the single-item path, then batch over both: only the
        // uncached item-2 may reach the wire (1 pre-cache request + 1 batch request; a batch
        // that ignored the cache would make 3).
        fetch_and_cache_cover(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1").await.unwrap();
        fetch_and_cache_covers(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, vec!["item-1".into(), "item-2".into()]).await;

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "the batch must serve the cached item from disk, not HTTP");
    }
}
