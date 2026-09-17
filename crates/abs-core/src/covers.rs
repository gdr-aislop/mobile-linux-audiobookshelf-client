//! Fetches and locally caches an item's cover image. Best-effort, same posture as
//! `progress_sync`'s reconciliation: a cover is cosmetic, never required for playback, so a
//! failure here is logged by the caller and treated as "no cover" rather than a playback error.
//! Uses a short timeout for the same reason — this must never add material delay to starting
//! playback.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::SqlitePool;

use abs_storage::AppPaths;

const COVER_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Returns the local path to the item's cached cover, fetching and writing it first if it isn't
/// already cached. Returns `None` on any failure (offline, 404, disk error, ...) — callers treat
/// "no cover" as a normal outcome, not something to surface to the user.
pub async fn fetch_and_cache_cover(
    paths: &AppPaths,
    pool: &SqlitePool,
    server_url: &str,
    access_token: &str,
    server_id: &str,
    item_id: &str,
) -> Option<PathBuf> {
    if let Some(cached) = already_cached(pool, server_id, item_id).await {
        return Some(cached);
    }

    let api = abs_api::Client::with_bearer_token_and_timeout(server_url, access_token, COVER_FETCH_TIMEOUT).ok()?;
    let cover = match api.get_item_cover(item_id).await {
        Ok(cover) => cover,
        Err(err) => {
            tracing::warn!(%err, item_id, "couldn't fetch cover art");
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

/// A cached path is only trusted if the file it points to still actually exists on disk — the
/// cache directory is evictable (`$XDG_CACHE_HOME`), so the recorded path can go stale without
/// this client's own doing.
async fn already_cached(pool: &SqlitePool, server_id: &str, item_id: &str) -> Option<PathBuf> {
    let item = abs_storage::repo::items::get(pool, server_id, item_id).await.ok()?;
    let existing = Path::new(item.cover_cache_path.as_deref()?).to_path_buf();
    tokio::fs::metadata(&existing).await.ok()?;
    Some(existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use abs_storage::repo::{accounts, items, libraries, servers};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool_with_synced_item(item_id: &str) -> (SqlitePool, String) {
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
        (pool, server_id)
    }

    fn test_paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
        (tmp, paths)
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_writes_the_file_with_the_right_extension() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/webp").set_body_bytes(vec![1, 2, 3]))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_item("item-1").await;

        let cached = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await;
        let cached = cached.expect("fetch should succeed");

        assert_eq!(cached.extension().unwrap(), "webp");
        assert_eq!(tokio::fs::read(&cached).await.unwrap(), vec![1, 2, 3]);
        let item = abs_storage::repo::items::get(&pool, &server_id, "item-1").await.unwrap();
        assert_eq!(item.cover_cache_path.as_deref(), Some(cached.to_str().unwrap()));
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_reuses_an_existing_cache_hit() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(vec![9, 9, 9]))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_item("item-1").await;

        let first = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await.unwrap();
        let second = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await.unwrap();
        assert_eq!(first, second);

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a second call should be a cache hit, not a second HTTP request");
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_returns_none_on_a_server_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/cover")).respond_with(ResponseTemplate::new(404)).mount(&mock_server).await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_item("item-1").await;

        let result = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fetch_and_cache_cover_refetches_if_the_cached_file_was_deleted() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/cover"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(vec![1]))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_item("item-1").await;

        let cached = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await.unwrap();
        tokio::fs::remove_file(&cached).await.unwrap();

        let refetched = fetch_and_cache_cover(&paths, &pool, &mock_server.uri(), "token", &server_id, "item-1").await;
        assert!(refetched.is_some(), "a deleted cache file must not be trusted as a hit");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
    }
}
