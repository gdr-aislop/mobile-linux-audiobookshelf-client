//! Items are cached from the server (like libraries), so writes are idempotent upserts.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::{Result, StorageError};
use crate::models::Item;

pub struct UpsertItem<'a> {
    pub id: &'a str,
    pub server_id: &'a str,
    pub library_id: &'a str,
    pub title: &'a str,
    pub author: Option<&'a str>,
    pub narrator: Option<&'a str>,
    pub description: Option<&'a str>,
    pub duration_seconds: f64,
    pub added_at: chrono::DateTime<Utc>,
}

pub async fn upsert(pool: &SqlitePool, item: UpsertItem<'_>) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO items
            (id, server_id, library_id, title, author, narrator, description,
             duration_seconds, added_at, synced_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(server_id, id) DO UPDATE SET
            library_id = excluded.library_id,
            title = excluded.title,
            author = excluded.author,
            narrator = excluded.narrator,
            description = excluded.description,
            duration_seconds = excluded.duration_seconds,
            added_at = excluded.added_at,
            synced_at = excluded.synced_at",
    )
    .bind(item.id)
    .bind(item.server_id)
    .bind(item.library_id)
    .bind(item.title)
    .bind(item.author)
    .bind(item.narrator)
    .bind(item.description)
    .bind(item.duration_seconds)
    .bind(item.added_at.to_rfc3339())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, server_id: &str, id: &str) -> Result<Item> {
    sqlx::query_as(
        "SELECT id, server_id, library_id, title, author, narrator, description,
                cover_cache_path, duration_seconds, added_at, synced_at
         FROM items WHERE server_id = ? AND id = ?",
    )
    .bind(server_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StorageError::NotFound(format!("item {id} on server {server_id}")))
}

pub async fn list_for_library(pool: &SqlitePool, server_id: &str, library_id: &str) -> Result<Vec<Item>> {
    let items = sqlx::query_as(
        "SELECT id, server_id, library_id, title, author, narrator, description,
                cover_cache_path, duration_seconds, added_at, synced_at
         FROM items WHERE server_id = ? AND library_id = ? ORDER BY added_at DESC",
    )
    .bind(server_id)
    .bind(library_id)
    .fetch_all(pool)
    .await?;
    Ok(items)
}

pub async fn set_cover_cache_path(
    pool: &SqlitePool,
    server_id: &str,
    id: &str,
    path: Option<&str>,
) -> Result<()> {
    let result = sqlx::query("UPDATE items SET cover_cache_path = ? WHERE server_id = ? AND id = ?")
        .bind(path)
        .bind(server_id)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound(format!("item {id} on server {server_id}")));
    }
    Ok(())
}

pub async fn remove(pool: &SqlitePool, server_id: &str, id: &str) -> Result<()> {
    let result = sqlx::query("DELETE FROM items WHERE server_id = ? AND id = ?")
        .bind(server_id)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound(format!("item {id} on server {server_id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{libraries, servers};

    async fn pool_with_library() -> (SqlitePool, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3"))
            .await
            .unwrap();
        std::mem::forget(tmp);
        let server_id = servers::add(&pool, "https://a.example").await.unwrap();
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
        (pool, server_id, "lib-1".to_string())
    }

    fn item<'a>(server_id: &'a str, library_id: &'a str, id: &'a str, title: &'a str) -> UpsertItem<'a> {
        UpsertItem {
            id,
            server_id,
            library_id,
            title,
            author: Some("Andy Weir"),
            narrator: Some("Ray Porter"),
            description: Some("A lone astronaut..."),
            duration_seconds: 58230.0,
            added_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let (pool, server_id, library_id) = pool_with_library().await;
        upsert(&pool, item(&server_id, &library_id, "item-1", "Project Hail Mary"))
            .await
            .unwrap();

        let found = get(&pool, &server_id, "item-1").await.unwrap();
        assert_eq!(found.title, "Project Hail Mary");
        assert_eq!(found.author.as_deref(), Some("Andy Weir"));
        assert_eq!(found.cover_cache_path, None);
    }

    #[tokio::test]
    async fn upsert_twice_updates_in_place() {
        let (pool, server_id, library_id) = pool_with_library().await;
        upsert(&pool, item(&server_id, &library_id, "item-1", "Old Title")).await.unwrap();
        upsert(&pool, item(&server_id, &library_id, "item-1", "New Title")).await.unwrap();

        let items = list_for_library(&pool, &server_id, &library_id).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "New Title");
    }

    #[tokio::test]
    async fn removing_a_library_cascades_to_its_items() {
        let (pool, server_id, library_id) = pool_with_library().await;
        upsert(&pool, item(&server_id, &library_id, "item-1", "Title")).await.unwrap();

        libraries::remove(&pool, &server_id, &library_id).await.unwrap();

        assert!(get(&pool, &server_id, "item-1").await.is_err());
    }

    #[tokio::test]
    async fn upsert_rejects_unknown_library() {
        let (pool, server_id, _library_id) = pool_with_library().await;
        let result = upsert(&pool, item(&server_id, "no-such-library", "item-1", "Title")).await;
        assert!(result.is_err(), "FK to (server_id, library_id) should reject this");
    }

    #[tokio::test]
    async fn set_cover_cache_path_updates_and_can_be_cleared() {
        let (pool, server_id, library_id) = pool_with_library().await;
        upsert(&pool, item(&server_id, &library_id, "item-1", "Title")).await.unwrap();

        set_cover_cache_path(&pool, &server_id, "item-1", Some("/cache/covers/item-1.jpg"))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, &server_id, "item-1").await.unwrap().cover_cache_path,
            Some("/cache/covers/item-1.jpg".to_string())
        );

        set_cover_cache_path(&pool, &server_id, "item-1", None).await.unwrap();
        assert_eq!(get(&pool, &server_id, "item-1").await.unwrap().cover_cache_path, None);
    }

    #[tokio::test]
    async fn list_for_library_orders_newest_first() {
        let (pool, server_id, library_id) = pool_with_library().await;
        let mut older = item(&server_id, &library_id, "old", "Older");
        older.added_at = Utc::now() - chrono::Duration::days(1);
        upsert(&pool, older).await.unwrap();
        upsert(&pool, item(&server_id, &library_id, "new", "Newer")).await.unwrap();

        let items = list_for_library(&pool, &server_id, &library_id).await.unwrap();
        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["Newer", "Older"]);
    }

    #[tokio::test]
    async fn remove_missing_item_is_not_found() {
        let (pool, server_id, _library_id) = pool_with_library().await;
        let err = remove(&pool, &server_id, "no-such-item").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }
}
