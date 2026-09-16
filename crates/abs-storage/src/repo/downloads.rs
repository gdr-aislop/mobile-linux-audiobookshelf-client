//! One row per downloaded chapter file. This is what backs Item Detail's per-chapter offline
//! markers, the download-scope picker's four options, "Clear downloaded chapters", and the
//! Home/Library offline-mode toggle's "downloaded fully or partially" filter.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::Result;
use crate::models::{Download, DownloadCompleteness};

pub async fn record(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    chapter_index: i64,
    file_path: &str,
    file_size_bytes: i64,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO downloads (server_id, item_id, chapter_index, file_path, file_size_bytes, downloaded_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(server_id, item_id, chapter_index) DO UPDATE SET
            file_path = excluded.file_path,
            file_size_bytes = excluded.file_size_bytes,
            downloaded_at = excluded.downloaded_at",
    )
    .bind(server_id)
    .bind(item_id)
    .bind(chapter_index)
    .bind(file_path)
    .bind(file_size_bytes)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<Vec<Download>> {
    let downloads = sqlx::query_as(
        "SELECT server_id, item_id, chapter_index, file_path, file_size_bytes, downloaded_at
         FROM downloads WHERE server_id = ? AND item_id = ? ORDER BY chapter_index ASC",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    Ok(downloads)
}

pub async fn is_chapter_downloaded(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    chapter_index: i64,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM downloads WHERE server_id = ? AND item_id = ? AND chapter_index = ?",
    )
    .bind(server_id)
    .bind(item_id)
    .bind(chapter_index)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// Whether an item is fully, partially, or not-at-all downloaded, compared against its actual
/// chapter count — the definition the Home/Library offline-mode toggle filters on.
pub async fn completeness(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<DownloadCompleteness> {
    let total_chapters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chapters WHERE server_id = ? AND item_id = ?",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_one(pool)
    .await?;

    let downloaded_chapters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM downloads WHERE server_id = ? AND item_id = ?",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_one(pool)
    .await?;

    Ok(match (downloaded_chapters, total_chapters) {
        (0, _) => DownloadCompleteness::None,
        (d, t) if t > 0 && d >= t => DownloadCompleteness::Full,
        _ => DownloadCompleteness::Partial,
    })
}

/// Item ids (scoped to one server) that are downloaded fully or partially — the query behind the
/// Home/Library offline-mode toggle's filtered view.
pub async fn downloaded_item_ids(pool: &SqlitePool, server_id: &str) -> Result<Vec<String>> {
    let ids = sqlx::query_scalar(
        "SELECT DISTINCT item_id FROM downloads WHERE server_id = ?",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// "Clear downloaded chapters": remove every download row for an item. Callers are responsible
/// for actually deleting the files on disk (this repo only tracks what's in the database) —
/// typically by listing them first via `list_for_item`.
pub async fn clear_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM downloads WHERE server_id = ? AND item_id = ?")
        .bind(server_id)
        .bind(item_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn remove_chapter(
    pool: &SqlitePool,
    server_id: &str,
    item_id: &str,
    chapter_index: i64,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM downloads WHERE server_id = ? AND item_id = ? AND chapter_index = ?",
    )
    .bind(server_id)
    .bind(item_id)
    .bind(chapter_index)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{chapters, items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_chapters(count: i64) -> (SqlitePool, String, String) {
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

        let new_chapters: Vec<_> = (0..count)
            .map(|i| chapters::NewChapter {
                title: "Chapter",
                start_seconds: i as f64 * 1200.0,
                end_seconds: (i + 1) as f64 * 1200.0,
            })
            .collect();
        chapters::replace_all(&pool, &server_id, "item-1", &new_chapters).await.unwrap();

        (pool, server_id, "item-1".to_string())
    }

    #[tokio::test]
    async fn record_then_list_round_trips() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 0, "/downloads/001-ch.mp3", 1_000_000)
            .await
            .unwrap();

        let downloads = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].chapter_index, 0);
        assert_eq!(downloads[0].file_size_bytes, 1_000_000);
    }

    #[tokio::test]
    async fn record_twice_for_same_chapter_updates_in_place() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 0, "/old/path.mp3", 100).await.unwrap();
        record(&pool, &server_id, &item_id, 0, "/new/path.mp3", 200).await.unwrap();

        let downloads = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(downloads.len(), 1, "re-downloading a chapter should not duplicate its row");
        assert_eq!(downloads[0].file_path, "/new/path.mp3");
    }

    #[tokio::test]
    async fn is_chapter_downloaded_reflects_recorded_chapters_only() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 1, "/downloads/002-ch.mp3", 1).await.unwrap();

        assert!(!is_chapter_downloaded(&pool, &server_id, &item_id, 0).await.unwrap());
        assert!(is_chapter_downloaded(&pool, &server_id, &item_id, 1).await.unwrap());
        assert!(!is_chapter_downloaded(&pool, &server_id, &item_id, 2).await.unwrap());
    }

    #[tokio::test]
    async fn completeness_is_none_with_no_downloads() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        assert_eq!(
            completeness(&pool, &server_id, &item_id).await.unwrap(),
            DownloadCompleteness::None
        );
    }

    #[tokio::test]
    async fn completeness_is_partial_with_some_but_not_all_chapters() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 0, "/p.mp3", 1).await.unwrap();
        assert_eq!(
            completeness(&pool, &server_id, &item_id).await.unwrap(),
            DownloadCompleteness::Partial
        );
    }

    #[tokio::test]
    async fn completeness_is_full_when_every_chapter_is_downloaded() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        for i in 0..3 {
            record(&pool, &server_id, &item_id, i, "/p.mp3", 1).await.unwrap();
        }
        assert_eq!(
            completeness(&pool, &server_id, &item_id).await.unwrap(),
            DownloadCompleteness::Full
        );
    }

    #[tokio::test]
    async fn downloaded_item_ids_includes_partial_downloads() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 0, "/p.mp3", 1).await.unwrap();

        let ids = downloaded_item_ids(&pool, &server_id).await.unwrap();
        assert_eq!(ids, vec![item_id]);
    }

    #[tokio::test]
    async fn downloaded_item_ids_excludes_items_with_no_downloads() {
        let (pool, server_id, _item_id) = pool_with_chapters(3).await;
        assert!(downloaded_item_ids(&pool, &server_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clear_for_item_removes_every_download_row() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        for i in 0..3 {
            record(&pool, &server_id, &item_id, i, "/p.mp3", 1).await.unwrap();
        }

        let removed = clear_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(removed, 3);
        assert!(list_for_item(&pool, &server_id, &item_id).await.unwrap().is_empty());
        assert_eq!(
            completeness(&pool, &server_id, &item_id).await.unwrap(),
            DownloadCompleteness::None
        );
    }

    #[tokio::test]
    async fn remove_chapter_removes_only_that_chapter() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        for i in 0..3 {
            record(&pool, &server_id, &item_id, i, "/p.mp3", 1).await.unwrap();
        }

        remove_chapter(&pool, &server_id, &item_id, 1).await.unwrap();

        assert!(is_chapter_downloaded(&pool, &server_id, &item_id, 0).await.unwrap());
        assert!(!is_chapter_downloaded(&pool, &server_id, &item_id, 1).await.unwrap());
        assert!(is_chapter_downloaded(&pool, &server_id, &item_id, 2).await.unwrap());
    }

    #[tokio::test]
    async fn removing_a_chapter_row_cascades_to_its_download() {
        let (pool, server_id, item_id) = pool_with_chapters(3).await;
        record(&pool, &server_id, &item_id, 0, "/p.mp3", 1).await.unwrap();

        // `replace_all` deletes every existing chapter row (even ones a new list happens to
        // reuse the same index for) before re-inserting, so any download referencing an old
        // chapter row must cascade-delete rather than silently surviving under the new one.
        chapters::replace_all(
            &pool,
            &server_id,
            &item_id,
            &[chapters::NewChapter { title: "Only", start_seconds: 0.0, end_seconds: 100.0 }],
        )
        .await
        .unwrap();

        assert!(list_for_item(&pool, &server_id, &item_id).await.unwrap().is_empty());
    }
}
