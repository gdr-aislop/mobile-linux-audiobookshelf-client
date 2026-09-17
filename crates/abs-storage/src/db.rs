use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::Result;

/// Open (creating if necessary) the SQLite database at `path` and run any pending migrations.
/// This is the one place a connection pool gets constructed — callers never build their own.
pub async fn connect_and_migrate(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        // WAL: readers never block on (or behind) the writer. Without this, the pool's
        // concurrent readers queue behind any write transaction with the busy handler, which
        // showed up in the wild as multi-second point SELECTs/INSERTs and a saturated pool
        // (every connection stuck waiting on the file lock) freezing the UI's async work.
        .journal_mode(SqliteJournalMode::Wal)
        // WAL's standard companion: fsync only at checkpoints, not per commit — durability
        // vs. speed tradeoff is a non-issue for cache-and-progress data.
        .synchronous(SqliteSynchronous::Normal)
        // Explicit rather than the default: a contended write waits (up to this long) instead
        // of erroring with "database is locked".
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_and_migrate_creates_the_database_file() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("nested").join("db.sqlite3");

        assert!(!db_path.exists());
        let _pool = connect_and_migrate(&db_path).await.expect("connect+migrate");
        assert!(db_path.exists(), "the db file should be created on disk");
    }

    #[tokio::test]
    async fn connect_and_migrate_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("db.sqlite3");

        let _pool1 = connect_and_migrate(&db_path).await.expect("first connect");
        // Re-opening and re-migrating an already-migrated database must not error.
        let _pool2 = connect_and_migrate(&db_path)
            .await
            .expect("second connect against the same, already-migrated db");
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3"))
            .await
            .unwrap();

        // Inserting a library referencing a server that doesn't exist must fail, proving
        // `foreign_keys(true)` actually took effect (SQLite ignores FK constraints by default).
        let result = sqlx::query(
            "INSERT INTO libraries (id, server_id, name, media_type, display_order, synced_at)
             VALUES ('lib', 'no-such-server', 'Lib', 'book', 1, '2024-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await;

        assert!(result.is_err(), "FK violation should be rejected");
    }

    #[tokio::test]
    async fn expected_tables_exist_after_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3"))
            .await
            .unwrap();

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx_%'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        for expected in [
            "servers", "accounts", "libraries", "items", "chapters", "progress", "tracks",
            "download_tracks", "settings", "bookmarks",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "expected table `{expected}` to exist, found: {tables:?}"
            );
        }
    }
}
