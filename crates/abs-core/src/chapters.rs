//! Persists an item's chapter list (already resolved by
//! `abs_core::streaming::resolve_stream_target`, riding free in the same `GET /api/items/:id`
//! response used to find the playable audio file) into local storage, so the chapters sheet has
//! something to show without a second network round-trip.

use sqlx::SqlitePool;

use crate::error::Result;

pub async fn sync_item_chapters(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    chapters: &[abs_api::ChapterRef],
) -> Result<()> {
    let rows: Vec<abs_storage::repo::chapters::NewChapter<'_>> = chapters
        .iter()
        .map(|c| abs_storage::repo::chapters::NewChapter {
            title: &c.title,
            start_seconds: c.start_seconds,
            end_seconds: c.end_seconds,
        })
        .collect();
    abs_storage::repo::chapters::replace_all(pool, server_id, item_id, &rows).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abs_storage::repo::{accounts, items, libraries, servers};

    async fn pool_with_synced_item(item_id: &str) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);

        let server_id = servers::add(&pool, "http://example.invalid").await.unwrap();
        accounts::add(&pool, &server_id, "jane", "token").await.unwrap();
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

    #[tokio::test]
    async fn sync_item_chapters_round_trips_through_storage() {
        let (pool, server_id) = pool_with_synced_item("item-1").await;
        let chapters = vec![
            abs_api::ChapterRef { title: "Intro".to_string(), start_seconds: 0.0, end_seconds: 60.0 },
            abs_api::ChapterRef { title: "Chapter 1".to_string(), start_seconds: 60.0, end_seconds: 300.0 },
        ];

        sync_item_chapters(&pool, &server_id, "item-1", &chapters).await.unwrap();

        let stored = abs_storage::repo::chapters::list_for_item(&pool, &server_id, "item-1").await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].title, "Intro");
        assert_eq!(stored[1].start_seconds, 60.0);
    }

    #[tokio::test]
    async fn sync_item_chapters_replaces_a_shrinking_list() {
        let (pool, server_id) = pool_with_synced_item("item-1").await;
        let three = vec![
            abs_api::ChapterRef { title: "A".to_string(), start_seconds: 0.0, end_seconds: 10.0 },
            abs_api::ChapterRef { title: "B".to_string(), start_seconds: 10.0, end_seconds: 20.0 },
            abs_api::ChapterRef { title: "C".to_string(), start_seconds: 20.0, end_seconds: 30.0 },
        ];
        sync_item_chapters(&pool, &server_id, "item-1", &three).await.unwrap();

        let one = vec![abs_api::ChapterRef { title: "Only".to_string(), start_seconds: 0.0, end_seconds: 30.0 }];
        sync_item_chapters(&pool, &server_id, "item-1", &one).await.unwrap();

        let stored = abs_storage::repo::chapters::list_for_item(&pool, &server_id, "item-1").await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].title, "Only");
    }
}
