//! Reconciles locally recorded playback progress against the server's, so progress made on
//! another device (or one of the official apps) shows up here too — not just this client's own
//! writes (`abs_core::streaming::sync_progress_to_server` handles the other direction: pushing a
//! local write up to the server).
//!
//! Every call here is best-effort and bounded: it uses a short timeout (much shorter than the
//! 15s default used for calls that are actually required to proceed, like resolving a stream
//! URL), and callers are expected to treat a failure — offline, a slow connection, a server
//! that's simply gone — as "nothing to reconcile, fall back to local storage" rather than letting
//! it block anything. Reading progress off the network is a nice-to-have, never a requirement.

use std::time::Duration;

use crate::error::{CoreError, Result};

/// Best-effort work should never be felt as a hang — this is deliberately much shorter than the
/// 15s default `abs_api::Client::with_bearer_token` uses for calls actually required to proceed
/// (e.g. resolving a playable URL).
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pulls a single item's progress from the server and, if the server's copy is newer than (or
/// there is no) local record, overwrites local storage with it. Called right before starting
/// playback, so resuming reflects the freshest progress across devices rather than only this
/// client's own last local write.
pub async fn reconcile_item_progress(
    pool: &sqlx::SqlitePool,
    connection: &crate::connection::ConnectionTarget,
    access_token: &str,
    account_id: &str,
    server_id: &str,
    item_id: &str,
) -> Result<()> {
    let api = connection
        .api_client_with_timeout(access_token, RECONCILE_TIMEOUT)
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    let Some(server_progress) =
        api.get_media_progress(item_id).await.map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?
    else {
        return Ok(());
    };

    apply_if_newer(pool, account_id, server_id, item_id, &server_progress).await
}

/// Bulk version of [`reconcile_item_progress`] for Home's "Continue Listening" shelf — one
/// `/api/me` call instead of one per item. Progress for an item this client hasn't synced into
/// its local `items` table yet (e.g. a library it hasn't opened) is skipped rather than erroring:
/// the local `progress` row has a foreign key on `items`, and there is nothing useful to show for
/// an item Home doesn't otherwise know about.
pub async fn reconcile_all_progress(
    pool: &sqlx::SqlitePool,
    connection: &crate::connection::ConnectionTarget,
    access_token: &str,
    account_id: &str,
    server_id: &str,
) -> Result<()> {
    let api = connection
        .api_client_with_timeout(access_token, RECONCILE_TIMEOUT)
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    let all_progress = api.get_all_media_progress().await.map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;

    for server_progress in &all_progress {
        if abs_storage::repo::items::get(pool, server_id, &server_progress.library_item_id).await.is_err() {
            continue;
        }
        apply_if_newer(pool, account_id, server_id, &server_progress.library_item_id, server_progress).await?;
    }
    Ok(())
}

async fn apply_if_newer(
    pool: &sqlx::SqlitePool,
    account_id: &str,
    server_id: &str,
    item_id: &str,
    server_progress: &abs_api::ServerProgress,
) -> Result<()> {
    let local = abs_storage::repo::progress::get(pool, account_id, server_id, item_id).await?;
    let server_updated_at = chrono::DateTime::from_timestamp_millis(server_progress.last_update_ms).unwrap_or_default();

    let should_overwrite = match &local {
        None => true,
        Some(local) => server_updated_at > local.updated_at,
    };
    if !should_overwrite {
        return Ok(());
    }

    // The server's own last-update time is preserved, not "now" — Home's "Continue Listening"
    // shelf orders by `updated_at`, and an imported record stamped with the import time would
    // make an item last listened to years ago look more recent than one listened to today.
    abs_storage::repo::progress::set_at(
        pool,
        account_id,
        server_id,
        item_id,
        server_progress.current_time_seconds,
        server_progress.is_finished,
        server_updated_at,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionTarget;
    use abs_storage::repo::{accounts, items, libraries, servers};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool_with_synced_item(server_url: &str, item_id: &str) -> (sqlx::SqlitePool, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);

        let server_id = servers::add(&pool, server_url).await.unwrap();
        let account_id = accounts::add(&pool, &server_id, "jane", "token123", None).await.unwrap();
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

        (pool, server_id, account_id)
    }

    #[tokio::test]
    async fn reconcile_item_progress_pulls_a_fresh_server_record_when_nothing_local_exists() {
        // Deliberately in the past: the imported row must carry the server's last-update time
        // (what "Continue Listening" orders by), not the moment of the import itself. Millis
        // precision, matching what `lastUpdate` can express over the wire.
        let last_update = chrono::DateTime::from_timestamp_millis(chrono::Utc::now().timestamp_millis() - 30 * 86_400_000).unwrap();
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraryItemId": "item-1",
                "currentTime": 55.0,
                "duration": 100.0,
                "isFinished": false,
                "lastUpdate": last_update.timestamp_millis(),
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id, account_id) = pool_with_synced_item(&mock_server.uri(), "item-1").await;
        reconcile_item_progress(&pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &account_id, &server_id, "item-1").await.unwrap();

        let progress = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-1").await.unwrap();
        let progress = progress.unwrap();
        assert_eq!(progress.current_time_seconds, 55.0);
        assert_eq!(progress.updated_at, last_update, "the server's last-update time must be kept, not the import time");
    }

    #[tokio::test]
    async fn reconcile_item_progress_with_no_server_record_leaves_local_storage_untouched() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/me/progress/item-1")).respond_with(ResponseTemplate::new(404)).mount(&mock_server).await;

        let (pool, server_id, account_id) = pool_with_synced_item(&mock_server.uri(), "item-1").await;
        abs_storage::repo::progress::set(&pool, &account_id, &server_id, "item-1", 12.0, false).await.unwrap();

        reconcile_item_progress(&pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &account_id, &server_id, "item-1").await.unwrap();

        let progress = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-1").await.unwrap();
        assert_eq!(progress.unwrap().current_time_seconds, 12.0, "no server record means nothing to reconcile");
    }

    #[tokio::test]
    async fn reconcile_item_progress_does_not_overwrite_a_newer_local_write() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraryItemId": "item-1",
                "currentTime": 5.0,
                "duration": 100.0,
                "isFinished": false,
                // Far in the past — any local write from "now" should win over this.
                "lastUpdate": 0,
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id, account_id) = pool_with_synced_item(&mock_server.uri(), "item-1").await;
        abs_storage::repo::progress::set(&pool, &account_id, &server_id, "item-1", 90.0, false).await.unwrap();

        reconcile_item_progress(&pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &account_id, &server_id, "item-1").await.unwrap();

        let progress = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-1").await.unwrap();
        assert_eq!(progress.unwrap().current_time_seconds, 90.0, "a stale server record must not clobber a newer local write");
    }

    #[tokio::test]
    async fn reconcile_item_progress_overwrites_a_stale_local_write() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "libraryItemId": "item-1",
                "currentTime": 200.0,
                "duration": 300.0,
                "isFinished": false,
                "lastUpdate": chrono::Utc::now().timestamp_millis() + 60_000,
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id, account_id) = pool_with_synced_item(&mock_server.uri(), "item-1").await;
        abs_storage::repo::progress::set(&pool, &account_id, &server_id, "item-1", 10.0, false).await.unwrap();

        reconcile_item_progress(&pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &account_id, &server_id, "item-1").await.unwrap();

        let progress = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-1").await.unwrap();
        assert_eq!(progress.unwrap().current_time_seconds, 200.0, "a newer server record should win over a stale local write");
    }

    #[tokio::test]
    async fn reconcile_item_progress_fails_fast_when_the_server_is_unreachable() {
        // No mock server at all — a closed port refuses immediately rather than hanging, so this
        // also verifies the call doesn't silently succeed against nothing.
        let result = reconcile_item_progress(&sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(), &ConnectionTarget::direct("http://127.0.0.1:1"), "token", "acc-1", "srv-1", "item-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn reconcile_all_progress_skips_items_not_synced_locally() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "mediaProgress": [
                    { "libraryItemId": "item-unknown", "currentTime": 1.0, "duration": 10.0, "isFinished": false, "lastUpdate": 1 },
                    { "libraryItemId": "item-1", "currentTime": 42.0, "duration": 100.0, "isFinished": false, "lastUpdate": chrono::Utc::now().timestamp_millis() },
                ]
            })))
            .mount(&mock_server)
            .await;

        let (pool, server_id, account_id) = pool_with_synced_item(&mock_server.uri(), "item-1").await;
        reconcile_all_progress(&pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &account_id, &server_id).await.unwrap();

        let known = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-1").await.unwrap();
        assert_eq!(known.unwrap().current_time_seconds, 42.0);
        let unknown = abs_storage::repo::progress::get(&pool, &account_id, &server_id, "item-unknown").await.unwrap();
        assert!(unknown.is_none(), "progress for an item Home hasn't synced yet should be skipped, not error");
    }
}
