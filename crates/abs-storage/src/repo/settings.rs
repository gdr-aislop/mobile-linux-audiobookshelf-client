//! Generic key/value settings store backing `docs/design/ui-spec.md`'s Settings screen and the
//! Library view-options sheet (default speed, skip intervals, sleep-timer default, theme,
//! Wi-Fi-only downloads, hide-finished, grouping/sort choices, ...). Callers in `abs-core` own
//! the actual key names and typed parsing; this module just persists strings.

use sqlx::SqlitePool;

use crate::error::Result;

pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let value = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

pub async fn get_or(pool: &SqlitePool, key: &str, default: &str) -> Result<String> {
    Ok(get(pool, key).await?.unwrap_or_else(|| default.to_string()))
}

pub async fn remove(pool: &SqlitePool, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;

    async fn pool() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3"))
            .await
            .unwrap();
        std::mem::forget(tmp);
        pool
    }

    #[tokio::test]
    async fn get_missing_key_is_none() {
        let pool = pool().await;
        assert_eq!(get(&pool, "no.such.key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let pool = pool().await;
        set(&pool, "playback.default_speed", "1.2").await.unwrap();
        assert_eq!(
            get(&pool, "playback.default_speed").await.unwrap(),
            Some("1.2".to_string())
        );
    }

    #[tokio::test]
    async fn set_overwrites_an_existing_value() {
        let pool = pool().await;
        set(&pool, "appearance.theme", "system").await.unwrap();
        set(&pool, "appearance.theme", "dark").await.unwrap();
        assert_eq!(
            get(&pool, "appearance.theme").await.unwrap(),
            Some("dark".to_string())
        );
    }

    #[tokio::test]
    async fn get_or_returns_default_when_unset() {
        let pool = pool().await;
        assert_eq!(
            get_or(&pool, "playback.skip_forward_seconds", "30")
                .await
                .unwrap(),
            "30"
        );
    }

    #[tokio::test]
    async fn get_or_returns_stored_value_when_set() {
        let pool = pool().await;
        set(&pool, "playback.skip_forward_seconds", "45")
            .await
            .unwrap();
        assert_eq!(
            get_or(&pool, "playback.skip_forward_seconds", "30")
                .await
                .unwrap(),
            "45"
        );
    }

    #[tokio::test]
    async fn remove_deletes_the_key() {
        let pool = pool().await;
        set(&pool, "library.downloaded_only", "true").await.unwrap();
        remove(&pool, "library.downloaded_only").await.unwrap();
        assert_eq!(get(&pool, "library.downloaded_only").await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_missing_key_does_not_error() {
        let pool = pool().await;
        remove(&pool, "no.such.key").await.unwrap();
    }
}
