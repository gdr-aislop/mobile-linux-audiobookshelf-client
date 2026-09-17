use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::Result;
use crate::models::Progress;

/// Record (or update) playback position for an item, for a given account. This is the local
/// write path — syncing it up to the server, and reconciling with the server's own progress
/// record, is `abs-core`'s job, not this repo's.
pub async fn set(
    pool: &SqlitePool,
    account_id: &str,
    server_id: &str,
    item_id: &str,
    current_time_seconds: f64,
    is_finished: bool,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO progress (account_id, server_id, item_id, current_time_seconds, is_finished, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(account_id, server_id, item_id) DO UPDATE SET
            current_time_seconds = excluded.current_time_seconds,
            is_finished = excluded.is_finished,
            updated_at = excluded.updated_at",
    )
    .bind(account_id)
    .bind(server_id)
    .bind(item_id)
    .bind(current_time_seconds)
    .bind(is_finished)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(
    pool: &SqlitePool,
    account_id: &str,
    server_id: &str,
    item_id: &str,
) -> Result<Option<Progress>> {
    let progress = sqlx::query_as(
        "SELECT account_id, server_id, item_id, current_time_seconds, is_finished, updated_at
         FROM progress WHERE account_id = ? AND server_id = ? AND item_id = ?",
    )
    .bind(account_id)
    .bind(server_id)
    .bind(item_id)
    .fetch_optional(pool)
    .await?;
    Ok(progress)
}

/// Items with any progress at all for an account, most-recently-updated first — backs the Home
/// screen's "Continue listening" shelf. Callers filter out finished items themselves (or not,
/// per the "Hide finished" view option) rather than this query hardcoding that policy.
pub async fn list_recent_for_account(
    pool: &SqlitePool,
    account_id: &str,
    limit: i64,
) -> Result<Vec<Progress>> {
    let progress = sqlx::query_as(
        "SELECT account_id, server_id, item_id, current_time_seconds, is_finished, updated_at
         FROM progress WHERE account_id = ? ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(progress)
}

pub async fn remove(pool: &SqlitePool, account_id: &str, server_id: &str, item_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM progress WHERE account_id = ? AND server_id = ? AND item_id = ?")
        .bind(account_id)
        .bind(server_id)
        .bind(item_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{accounts, items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_item_and_account() -> (SqlitePool, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3"))
            .await
            .unwrap();
        std::mem::forget(tmp);
        let server_id = servers::add(&pool, "https://a.example").await.unwrap();
        let account_id = accounts::add(&pool, &server_id, "jane", "tok", None).await.unwrap();
        libraries::upsert(
            &pool,
            libraries::UpsertLibrary {
                id: "lib-1",
                server_id: &server_id,
                name: "Audiobooks",
                media_type: "book",
                icon: None,
                display_order: 1,
            },
        )
        .await
        .unwrap();
        items::upsert(
            &pool,
            items::UpsertItem {
                id: "item-1",
                server_id: &server_id,
                library_id: "lib-1",
                title: "Project Hail Mary",
                author: None,
                narrator: None,
                description: None,
                duration_seconds: 3600.0,
                added_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        (pool, server_id, account_id, "item-1".to_string())
    }

    #[tokio::test]
    async fn get_with_no_progress_is_none() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        assert!(get(&pool, &account_id, &server_id, &item_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        set(&pool, &account_id, &server_id, &item_id, 612.0, false).await.unwrap();

        let progress = get(&pool, &account_id, &server_id, &item_id).await.unwrap().unwrap();
        assert_eq!(progress.current_time_seconds, 612.0);
        assert!(!progress.is_finished);
    }

    #[tokio::test]
    async fn set_twice_updates_in_place() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        set(&pool, &account_id, &server_id, &item_id, 100.0, false).await.unwrap();
        set(&pool, &account_id, &server_id, &item_id, 3600.0, true).await.unwrap();

        let progress = get(&pool, &account_id, &server_id, &item_id).await.unwrap().unwrap();
        assert_eq!(progress.current_time_seconds, 3600.0);
        assert!(progress.is_finished);
    }

    #[tokio::test]
    async fn progress_is_scoped_per_account() {
        let (pool, server_id, account_a, item_id) = pool_with_item_and_account().await;
        let account_b = accounts::add(&pool, &server_id, "jack", "tok2", None).await.unwrap();

        set(&pool, &account_a, &server_id, &item_id, 100.0, false).await.unwrap();
        set(&pool, &account_b, &server_id, &item_id, 2000.0, false).await.unwrap();

        assert_eq!(
            get(&pool, &account_a, &server_id, &item_id).await.unwrap().unwrap().current_time_seconds,
            100.0
        );
        assert_eq!(
            get(&pool, &account_b, &server_id, &item_id).await.unwrap().unwrap().current_time_seconds,
            2000.0
        );
    }

    #[tokio::test]
    async fn list_recent_for_account_orders_newest_first_and_respects_limit() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        items::upsert(
            &pool,
            items::UpsertItem {
                id: "item-2",
                server_id: &server_id,
                library_id: "lib-1",
                title: "Another Book",
                author: None,
                narrator: None,
                description: None,
                duration_seconds: 1000.0,
                added_at: Utc::now(),
            },
        )
        .await
        .unwrap();

        set(&pool, &account_id, &server_id, &item_id, 100.0, false).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        set(&pool, &account_id, &server_id, "item-2", 200.0, false).await.unwrap();

        let recent = list_recent_for_account(&pool, &account_id, 10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].item_id, "item-2", "most recently updated should be first");

        let limited = list_recent_for_account(&pool, &account_id, 1).await.unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[tokio::test]
    async fn removing_an_account_cascades_to_its_progress() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        set(&pool, &account_id, &server_id, &item_id, 100.0, false).await.unwrap();

        accounts::remove(&pool, &account_id).await.unwrap();

        // The account is gone, so there's no valid account_id to query with, but we can at
        // least confirm the row was removed via a raw count.
        let tmp_pool = &pool;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM progress")
            .fetch_one(tmp_pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn remove_deletes_a_progress_row() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        set(&pool, &account_id, &server_id, &item_id, 100.0, false).await.unwrap();
        remove(&pool, &account_id, &server_id, &item_id).await.unwrap();
        assert!(get(&pool, &account_id, &server_id, &item_id).await.unwrap().is_none());
    }
}
