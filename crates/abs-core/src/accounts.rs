//! Multi-server/account management: adding a server and logging in, switching the active
//! account, and signing out. Thin orchestration over `abs-storage::repo::{servers, accounts}`
//! and `abs_api::Client::login` — the interesting invariants (exactly one active account,
//! cascade cleanup) already live in the storage layer's tests; this module's tests focus on the
//! login-then-persist flow and its failure modes.

use abs_storage::repo::{accounts, servers};
use sqlx::SqlitePool;

use crate::error::Result;

pub struct AddedAccount {
    pub server_id: String,
    pub account_id: String,
}

/// Register a new server and log into it in one step — the flow behind the Welcome/Server login
/// screen. On login failure, the newly-created server row is rolled back rather than left behind
/// as an orphaned, credential-less entry. The new account becomes the active one: this is the
/// only way to add an account today, so leaving it inactive would mean the app forgets you're
/// signed in the next time it starts and shows the Welcome screen again.
pub async fn add_server_and_login(
    pool: &SqlitePool,
    url: &str,
    username: &str,
    password: &str,
) -> Result<AddedAccount> {
    let server_id = servers::add(pool, url).await?;

    let client = abs_api::Client::new(url);
    let login_result = match client.login(username, password).await {
        Ok(result) => result,
        Err(err) => {
            // Don't leave a credential-less server behind after a failed login.
            let _ = servers::remove(pool, &server_id).await;
            return Err(err.into());
        }
    };

    let account_id = accounts::add(pool, &server_id, &login_result.username, &login_result.access_token).await?;
    accounts::set_active(pool, &account_id).await?;

    Ok(AddedAccount { server_id, account_id })
}

/// Switch the active account, which may belong to a different server than whatever was
/// previously active — this is the "Switch to this server" action from Settings' per-server menu.
pub async fn switch_active_account(pool: &SqlitePool, account_id: &str) -> Result<()> {
    accounts::set_active(pool, account_id).await?;
    Ok(())
}

/// Sign out of one account. Its local progress rows cascade-delete with it (see the storage
/// layer's migration); the server itself and any other accounts on it are untouched.
pub async fn sign_out(pool: &SqlitePool, account_id: &str) -> Result<()> {
    accounts::remove(pool, account_id).await?;
    Ok(())
}

/// Remove a server entirely — "Remove Server" from Settings' per-server menu. Cascades to every
/// account, library, item, and download record for that server.
pub async fn remove_server(pool: &SqlitePool, server_id: &str) -> Result<()> {
    servers::remove(pool, server_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;
    use abs_storage::connect_and_migrate;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn pool() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        pool
    }

    fn mock_login_success() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user": {
                "id": "user-1",
                "username": "jane",
                "accessToken": "abc123",
                "refreshToken": "refresh456",
            }
        }))
    }

    #[tokio::test]
    async fn add_server_and_login_persists_server_and_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2")
            .await
            .unwrap();

        let stored_server = servers::get(&pool, &added.server_id).await.unwrap();
        assert_eq!(stored_server.url, server.uri());

        let stored_account = accounts::get(&pool, &added.account_id).await.unwrap();
        assert_eq!(stored_account.username, "jane");
        assert_eq!(stored_account.token, "abc123");
        assert!(stored_account.is_active, "the newly added account should become active");
    }

    #[tokio::test]
    async fn failed_login_does_not_leave_an_orphaned_server_row() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(ResponseTemplate::new(401)).mount(&server).await;

        let pool = pool().await;
        let result = add_server_and_login(&pool, &server.uri(), "jane", "wrong").await;

        assert!(matches!(result, Err(CoreError::Login(_))));
        assert!(servers::list(&pool).await.unwrap().is_empty(), "no server row should remain");
    }

    #[tokio::test]
    async fn switch_active_account_delegates_to_storage() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();

        switch_active_account(&pool, &added.account_id).await.unwrap();
        assert!(accounts::get(&pool, &added.account_id).await.unwrap().is_active);
    }

    #[tokio::test]
    async fn sign_out_removes_the_account_but_not_the_server() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();

        sign_out(&pool, &added.account_id).await.unwrap();

        assert!(accounts::get(&pool, &added.account_id).await.is_err());
        assert!(servers::get(&pool, &added.server_id).await.is_ok(), "server should survive");
    }

    #[tokio::test]
    async fn remove_server_cascades_to_its_accounts() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();

        remove_server(&pool, &added.server_id).await.unwrap();

        assert!(servers::get(&pool, &added.server_id).await.is_err());
        assert!(accounts::get(&pool, &added.account_id).await.is_err());
    }
}
