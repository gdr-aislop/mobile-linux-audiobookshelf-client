use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::Result;

/// Record a bookmark at a position in an item, for a given account. Local-only — no server sync,
/// no `list`/`get` — nothing in the app reads these back yet (the player screen's "Add bookmark"
/// menu item is a write-only confirmation-toast action, per `docs/design/ui-spec.md`).
pub async fn add(
    pool: &SqlitePool,
    account_id: &str,
    server_id: &str,
    item_id: &str,
    position_seconds: f64,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO bookmarks (id, account_id, server_id, item_id, position_seconds, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(account_id)
    .bind(server_id)
    .bind(item_id)
    .bind(position_seconds)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{accounts, items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_item_and_account() -> (SqlitePool, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
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
    async fn add_inserts_a_row() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        add(&pool, &account_id, &server_id, &item_id, 612.5).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn add_returns_a_unique_id_each_time() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        let id1 = add(&pool, &account_id, &server_id, &item_id, 10.0).await.unwrap();
        let id2 = add(&pool, &account_id, &server_id, &item_id, 20.0).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn removing_an_account_cascades_to_its_bookmarks() {
        let (pool, server_id, account_id, item_id) = pool_with_item_and_account().await;
        add(&pool, &account_id, &server_id, &item_id, 100.0).await.unwrap();

        accounts::remove(&pool, &account_id).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
