//! Chapters are synced as a whole list per item (Audiobookshelf returns the full chapter list on
//! every item fetch, not incrementally), so `replace_all` is the write primitive rather than a
//! per-chapter upsert: it deletes and re-inserts within a transaction, which also naturally
//! handles a chapter list shrinking (re-chaptered media) without leaving stale rows behind.

use sqlx::SqlitePool;

use crate::error::Result;
use crate::models::Chapter;

pub struct NewChapter<'a> {
    pub title: &'a str,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

pub async fn replace_all(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    chapters: &[NewChapter<'_>],
) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM chapters WHERE server_id = ? AND item_id = ?")
        .bind(server_id)
        .bind(item_id)
        .execute(&mut *tx)
        .await?;

    for (index, chapter) in chapters.iter().enumerate() {
        sqlx::query(
            "INSERT INTO chapters (server_id, item_id, chapter_index, title, start_seconds, end_seconds)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(server_id)
        .bind(item_id)
        .bind(index as i64)
        .bind(chapter.title)
        .bind(chapter.start_seconds)
        .bind(chapter.end_seconds)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn list_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<Vec<Chapter>> {
    let chapters = sqlx::query_as(
        "SELECT server_id, item_id, chapter_index, title, start_seconds, end_seconds
         FROM chapters WHERE server_id = ? AND item_id = ? ORDER BY chapter_index ASC",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    Ok(chapters)
}

/// The chapter whose [start, end) range contains `position_seconds`, if any. Used to resolve
/// "current chapter" for the download-scope picker and the now-playing chapter label.
pub async fn at_position(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    position_seconds: f64,
) -> Result<Option<Chapter>> {
    let chapter = sqlx::query_as(
        "SELECT server_id, item_id, chapter_index, title, start_seconds, end_seconds
         FROM chapters
         WHERE server_id = ? AND item_id = ? AND start_seconds <= ? AND ? < end_seconds
         LIMIT 1",
    )
    .bind(server_id)
    .bind(item_id)
    .bind(position_seconds)
    .bind(position_seconds)
    .fetch_optional(pool)
    .await?;
    Ok(chapter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_item() -> (SqlitePool, String, String) {
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
        (pool, server_id, "item-1".to_string())
    }

    fn three_chapters() -> Vec<NewChapter<'static>> {
        vec![
            NewChapter { title: "Ch 1", start_seconds: 0.0, end_seconds: 1200.0 },
            NewChapter { title: "Ch 2", start_seconds: 1200.0, end_seconds: 2400.0 },
            NewChapter { title: "Ch 3", start_seconds: 2400.0, end_seconds: 3600.0 },
        ]
    }

    #[tokio::test]
    async fn replace_all_then_list_round_trips_in_order() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        let chapters = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        let titles: Vec<_> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["Ch 1", "Ch 2", "Ch 3"]);
        assert_eq!(chapters[0].chapter_index, 0);
        assert_eq!(chapters[2].chapter_index, 2);
    }

    #[tokio::test]
    async fn replace_all_removes_chapters_no_longer_present() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        // Re-chaptered down to a single chapter.
        replace_all(
            &pool,
            &server_id,
            &item_id,
            &[NewChapter { title: "Whole Book", start_seconds: 0.0, end_seconds: 3600.0 }],
        )
        .await
        .unwrap();

        let chapters = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "Whole Book");
    }

    #[tokio::test]
    async fn removing_an_item_cascades_to_its_chapters() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        items::remove(&pool, &server_id, &item_id).await.unwrap();

        assert!(list_for_item(&pool, &server_id, &item_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn at_position_finds_the_containing_chapter() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        let chapter = at_position(&pool, &server_id, &item_id, 1500.0).await.unwrap().unwrap();
        assert_eq!(chapter.title, "Ch 2");
    }

    #[tokio::test]
    async fn at_position_is_inclusive_of_chapter_start() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        let chapter = at_position(&pool, &server_id, &item_id, 1200.0).await.unwrap().unwrap();
        assert_eq!(chapter.title, "Ch 2", "exactly at a boundary belongs to the next chapter");
    }

    #[tokio::test]
    async fn at_position_beyond_the_end_finds_nothing() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();

        assert!(at_position(&pool, &server_id, &item_id, 9999.0).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn replace_all_with_empty_list_clears_chapters() {
        let (pool, server_id, item_id) = pool_with_item().await;
        replace_all(&pool, &server_id, &item_id, &three_chapters()).await.unwrap();
        replace_all(&pool, &server_id, &item_id, &[]).await.unwrap();

        assert!(list_for_item(&pool, &server_id, &item_id).await.unwrap().is_empty());
    }
}
