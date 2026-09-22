//! Cached per-item track metadata (`ino`, duration, book-level offset) — mirrors
//! `abs_core::streaming::StreamTrack`, persisted so download planning and (later) offline
//! availability checks don't need a network round trip. Tracks are synced as a whole list per item
//! (the same shape `resolve_stream_target` already returns in full every time), so `upsert_all` is
//! the write primitive — same "delete and re-insert in a transaction" reasoning as
//! `repo::chapters::replace_all`.

use sqlx::SqlitePool;

use crate::error::Result;
use crate::models::Track;

pub struct NewTrack<'a> {
    pub ino: &'a str,
    pub duration_seconds: f64,
    pub offset_seconds: f64,
    /// The server-reported file size, when the item's metadata carries it — what the download
    /// sheet's size estimates are computed from. `None` just means "unknown", never an error.
    pub size_bytes: Option<u64>,
}

pub async fn upsert_all(pool: &SqlitePool, server_id: &str, item_id: &str, tracks: &[NewTrack<'_>]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM tracks WHERE server_id = ? AND item_id = ?")
        .bind(server_id)
        .bind(item_id)
        .execute(&mut *tx)
        .await?;

    for (index, track) in tracks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO tracks (server_id, item_id, ino, track_index, duration_seconds, offset_seconds, size_bytes)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(server_id)
        .bind(item_id)
        .bind(track.ino)
        .bind(index as i64)
        .bind(track.duration_seconds)
        .bind(track.offset_seconds)
        .bind(track.size_bytes.map(|s| s as i64))
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn list_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<Vec<Track>> {
    let tracks = sqlx::query_as(
        "SELECT server_id, item_id, ino, track_index, duration_seconds, offset_seconds, size_bytes
         FROM tracks WHERE server_id = ? AND item_id = ? ORDER BY track_index ASC",
    )
    .bind(server_id)
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;
    use crate::repo::{items, libraries, servers};
    use chrono::Utc;

    async fn pool_with_item() -> (SqlitePool, String, String) {
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
                series_name: None,
                genres: &[],
            },
        )
        .await
        .unwrap();
        (pool, server_id, "item-1".to_string())
    }

    fn two_tracks() -> Vec<NewTrack<'static>> {
        vec![
            NewTrack { ino: "ino-1", duration_seconds: 1800.0, offset_seconds: 0.0, size_bytes: Some(1_500_000) },
            NewTrack { ino: "ino-2", duration_seconds: 1800.0, offset_seconds: 1800.0, size_bytes: None },
        ]
    }

    #[tokio::test]
    async fn upsert_all_then_list_round_trips_in_order() {
        let (pool, server_id, item_id) = pool_with_item().await;
        upsert_all(&pool, &server_id, &item_id, &two_tracks()).await.unwrap();

        let tracks = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].ino, "ino-1");
        assert_eq!(tracks[0].track_index, 0);
        assert_eq!(tracks[0].size_bytes, Some(1_500_000));
        assert_eq!(tracks[1].ino, "ino-2");
        assert_eq!(tracks[1].offset_seconds, 1800.0);
        assert_eq!(tracks[1].size_bytes, None, "a missing server size is unknown, not an error");
    }

    #[tokio::test]
    async fn upsert_all_removes_tracks_no_longer_present() {
        let (pool, server_id, item_id) = pool_with_item().await;
        upsert_all(&pool, &server_id, &item_id, &two_tracks()).await.unwrap();

        upsert_all(&pool, &server_id, &item_id, &[NewTrack { ino: "ino-1", duration_seconds: 3600.0, offset_seconds: 0.0, size_bytes: None }]).await.unwrap();

        let tracks = list_for_item(&pool, &server_id, &item_id).await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].ino, "ino-1");
    }

    #[tokio::test]
    async fn removing_an_item_cascades_to_its_tracks() {
        let (pool, server_id, item_id) = pool_with_item().await;
        upsert_all(&pool, &server_id, &item_id, &two_tracks()).await.unwrap();

        items::remove(&pool, &server_id, &item_id).await.unwrap();

        assert!(list_for_item(&pool, &server_id, &item_id).await.unwrap().is_empty());
    }
}
