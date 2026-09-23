//! Per-series book membership, cached from `GET /api/libraries/{id}/series` — the only place
//! Audiobookshelf's API exposes a book's real position ("sequence") within a series. A whole
//! library's series list is fetched in one call, so `replace_all_for_library` is the write
//! primitive — same "delete and re-insert in a transaction" reasoning as `repo::chapters::replace_all`,
//! scoped per `library_id` rather than per item this time.

use sqlx::SqlitePool;

use crate::error::Result;

pub struct NewSeriesBook<'a> {
    pub series_id: &'a str,
    pub series_name: &'a str,
    pub item_id: &'a str,
    pub sequence: Option<&'a str>,
}

pub async fn replace_all_for_library(pool: &SqlitePool, server_id: &str, library_id: &str, rows: &[NewSeriesBook<'_>]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM series_books WHERE server_id = ? AND library_id = ?")
        .bind(server_id)
        .bind(library_id)
        .execute(&mut *tx)
        .await?;

    for row in rows {
        sqlx::query(
            "INSERT INTO series_books (server_id, library_id, series_id, series_name, item_id, sequence)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(server_id)
        .bind(library_id)
        .bind(row.series_id)
        .bind(row.series_name)
        .bind(row.item_id)
        .bind(row.sequence)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// This item's own series/sequence, plus the series' total book count. `None` if the item was
/// never synced into any series (no series, or the library's series list hasn't been fetched
/// yet).
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesBookInfo {
    pub series_name: String,
    pub sequence: Option<String>,
    pub total_books: i64,
}

/// `total_books` is `max(how many books of this series are synced, the highest `sequence` number
/// among them)`, not a plain count — a library can hold only a sparse subset of a series (e.g.
/// books 2, 5 and 7 of a longer series), and "5 of 7" (7 being the highest known position, not
/// "3", the count of books actually owned) is what a reader expects "book 5" to mean. A
/// non-numeric or fractional `sequence` (e.g. `"5.5"` for an inserted novella) that can't be
/// parsed as a number just doesn't contribute to that maximum; the count alone still applies.
pub async fn find_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<Option<SeriesBookInfo>> {
    let Some((series_id, series_name, sequence)): Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT series_id, series_name, sequence FROM series_books WHERE server_id = ? AND item_id = ? LIMIT 1",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let sequences: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT sequence FROM series_books WHERE server_id = ? AND series_id = ?")
            .bind(server_id)
            .bind(&series_id)
            .fetch_all(pool)
            .await?;

    let count = sequences.len() as i64;
    let max_known_position = sequences
        .iter()
        .filter_map(|(s,)| s.as_deref())
        .filter_map(|s| s.parse::<f64>().ok())
        .fold(0.0_f64, f64::max);
    let total_books = count.max(max_known_position.ceil() as i64);

    Ok(Some(SeriesBookInfo { series_name, sequence, total_books }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_items(item_ids: &[&str]) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = servers::add(&pool, "https://a.example").await.unwrap();
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
                    title: "Some Book",
                    author: None,
                    narrator: None,
                    description: None,
                    duration_seconds: 3600.0,
                    added_at: Utc::now(),
                    series_name: None,
                    genres: &[],
                },
            )
            .await
            .unwrap();
        }
        (pool, server_id)
    }

    #[tokio::test]
    async fn replace_all_then_find_for_item_round_trips() {
        let (pool, server_id) = pool_with_items(&["item-1", "item-2", "item-3"]).await;
        replace_all_for_library(
            &pool,
            &server_id,
            "lib-1",
            &[
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-1", sequence: Some("1") },
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-2", sequence: Some("2") },
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-3", sequence: Some("2.5") },
            ],
        )
        .await
        .unwrap();

        let info = find_for_item(&pool, &server_id, "item-2").await.unwrap().unwrap();
        assert_eq!(info.series_name, "Foundation");
        assert_eq!(info.sequence, Some("2".to_string()));
        assert_eq!(info.total_books, 3, "the series has 3 synced books, regardless of which one we looked up");
    }

    /// Regression test: a library that only owns books 2, 5 and 7 of a longer series must report
    /// "5/7" for book 5 — `total_books` is the highest known `sequence` (7), not the count of
    /// synced books (3), which would misleadingly read "5/3".
    #[tokio::test]
    async fn total_books_uses_the_highest_known_sequence_for_a_sparse_series() {
        let (pool, server_id) = pool_with_items(&["item-2", "item-5", "item-7"]).await;
        replace_all_for_library(
            &pool,
            &server_id,
            "lib-1",
            &[
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-2", sequence: Some("2") },
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-5", sequence: Some("5") },
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-7", sequence: Some("7") },
            ],
        )
        .await
        .unwrap();

        let info = find_for_item(&pool, &server_id, "item-5").await.unwrap().unwrap();
        assert_eq!(info.sequence, Some("5".to_string()));
        assert_eq!(info.total_books, 7, "3 books are synced, but book 7 is known to exist, so the total must be 7, not 3");
    }

    #[tokio::test]
    async fn find_for_item_is_none_for_an_unsynced_or_seriesless_item() {
        let (pool, server_id) = pool_with_items(&["item-1"]).await;
        assert!(find_for_item(&pool, &server_id, "item-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn replace_all_running_twice_does_not_duplicate_or_leave_stale_rows() {
        let (pool, server_id) = pool_with_items(&["item-1", "item-2"]).await;
        replace_all_for_library(
            &pool,
            &server_id,
            "lib-1",
            &[
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-1", sequence: Some("1") },
                NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-2", sequence: Some("2") },
            ],
        )
        .await
        .unwrap();

        // The series shrinks to just one book on the next fetch.
        replace_all_for_library(
            &pool,
            &server_id,
            "lib-1",
            &[NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-1", sequence: Some("1") }],
        )
        .await
        .unwrap();

        let info = find_for_item(&pool, &server_id, "item-1").await.unwrap().unwrap();
        assert_eq!(info.total_books, 1, "the stale second row must not linger after a shrunk series list");
        assert!(find_for_item(&pool, &server_id, "item-2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn removing_an_item_cascades_to_its_series_books_row() {
        let (pool, server_id) = pool_with_items(&["item-1"]).await;
        replace_all_for_library(
            &pool,
            &server_id,
            "lib-1",
            &[NewSeriesBook { series_id: "series-1", series_name: "Foundation", item_id: "item-1", sequence: Some("1") }],
        )
        .await
        .unwrap();

        items::remove(&pool, &server_id, "item-1").await.unwrap();

        assert!(find_for_item(&pool, &server_id, "item-1").await.unwrap().is_none());
    }
}
