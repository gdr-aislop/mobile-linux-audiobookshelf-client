use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{Result, StorageError};
use crate::models::Server;

/// Register a new server connection. Returns the generated id.
pub async fn add(pool: &SqlitePool, url: &str) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO servers (id, url, custom_headers_json, disable_ssl_verify, created_at)
         VALUES (?, ?, '{}', 0, ?)",
    )
    .bind(&id)
    .bind(url)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;

    Ok(id)
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Server> {
    sqlx::query_as(
        "SELECT id, url, custom_headers_json, disable_ssl_verify, client_cert_path,
                local_network_address, user_agent, created_at
         FROM servers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StorageError::NotFound(format!("server {id}")))
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Server>> {
    let servers = sqlx::query_as(
        "SELECT id, url, custom_headers_json, disable_ssl_verify, client_cert_path,
                local_network_address, user_agent, created_at
         FROM servers ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(servers)
}

pub async fn remove(pool: &SqlitePool, id: &str) -> Result<()> {
    // ON DELETE CASCADE takes care of accounts/libraries/items/etc. for this server.
    let result = sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound(format!("server {id}")));
    }
    Ok(())
}

pub async fn set_disable_ssl_verify(pool: &SqlitePool, id: &str, disable: bool) -> Result<()> {
    let result = sqlx::query("UPDATE servers SET disable_ssl_verify = ? WHERE id = ?")
        .bind(disable)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound(format!("server {id}")));
    }
    Ok(())
}

pub async fn set_local_network_address(
    pool: &SqlitePool,
    id: &str,
    address: Option<&str>,
) -> Result<()> {
    let result = sqlx::query("UPDATE servers SET local_network_address = ? WHERE id = ?")
        .bind(address)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound(format!("server {id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_and_migrate;

    async fn pool() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        // Leak the tempdir for the pool's lifetime within the test; each test gets its own.
        let path = tmp.path().join("db.sqlite3");
        let pool = connect_and_migrate(&path).await.unwrap();
        std::mem::forget(tmp);
        pool
    }

    #[tokio::test]
    async fn add_then_get_round_trips() {
        let pool = pool().await;
        let id = add(&pool, "https://library.homeserver.dev").await.unwrap();

        let server = get(&pool, &id).await.unwrap();
        assert_eq!(server.id, id);
        assert_eq!(server.url, "https://library.homeserver.dev");
        assert!(!server.disable_ssl_verify);
        assert_eq!(server.client_cert_path, None);
    }

    #[tokio::test]
    async fn get_missing_server_returns_not_found() {
        let pool = pool().await;
        let err = get(&pool, "does-not-exist").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_returns_servers_in_creation_order() {
        let pool = pool().await;
        let first = add(&pool, "https://a.example").await.unwrap();
        let second = add(&pool, "https://b.example").await.unwrap();

        let servers = list(&pool).await.unwrap();
        let ids: Vec<_> = servers.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec![first, second]);
    }

    #[tokio::test]
    async fn remove_deletes_the_server() {
        let pool = pool().await;
        let id = add(&pool, "https://a.example").await.unwrap();
        remove(&pool, &id).await.unwrap();
        assert!(get(&pool, &id).await.is_err());
    }

    #[tokio::test]
    async fn remove_missing_server_is_an_error() {
        let pool = pool().await;
        let err = remove(&pool, "does-not-exist").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn set_disable_ssl_verify_updates_the_flag() {
        let pool = pool().await;
        let id = add(&pool, "https://a.example").await.unwrap();

        set_disable_ssl_verify(&pool, &id, true).await.unwrap();
        assert!(get(&pool, &id).await.unwrap().disable_ssl_verify);

        set_disable_ssl_verify(&pool, &id, false).await.unwrap();
        assert!(!get(&pool, &id).await.unwrap().disable_ssl_verify);
    }

    #[tokio::test]
    async fn set_local_network_address_can_be_cleared() {
        let pool = pool().await;
        let id = add(&pool, "https://a.example").await.unwrap();

        set_local_network_address(&pool, &id, Some("192.168.1.50"))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, &id).await.unwrap().local_network_address,
            Some("192.168.1.50".to_string())
        );

        set_local_network_address(&pool, &id, None).await.unwrap();
        assert_eq!(get(&pool, &id).await.unwrap().local_network_address, None);
    }
}
