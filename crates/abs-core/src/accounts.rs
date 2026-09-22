//! Multi-server/account management: adding a server and logging in, switching the active
//! account, and signing out. Thin orchestration over `abs-storage::repo::{servers, accounts}`
//! and `abs_api::Client::login` — the interesting invariants (exactly one active account,
//! cascade cleanup) already live in the storage layer's tests; this module's tests focus on the
//! login-then-persist flow and its failure modes.

use abs_storage::repo::{accounts, servers};
use abs_storage::AppPaths;
use sqlx::SqlitePool;

use crate::error::Result;

pub struct AddedAccount {
    pub server_id: String,
    pub account_id: String,
}

/// The previous session's identity, captured by the shell before sending the user back to the
/// login screen after an authorization failure. `relogin` compares what the user typed against
/// this to decide whether a successful login is a token rotation on the same account, a different
/// account on the same server, or a completely different server — each with different cleanup.
#[derive(Clone)]
pub struct ReloginSeed {
    pub server_id: String,
    pub account_id: String,
    pub url: String,
    pub username: String,
}

/// What a prospective re-login would do to the current session's local data. `None` means the
/// same URL and username as the seeded session — signing in again just rotates tokens and
/// touches nothing else.
pub enum ReplacementKind {
    /// Same server URL, different username: the server's cached libraries/items/covers/downloads
    /// stay valid (they're keyed by `server_id`), but the old account's progress and bookmarks
    /// cascade away with its row.
    SameServerNewAccount,
    /// Different server URL: everything cached for the old server — rows via FK cascade, and the
    /// on-disk cover/download files — is removed.
    NewServer,
}

/// URLs equal after trimming and dropping trailing slashes — the tolerance a user typing (or
/// pasting) the same address slightly differently should get. Scheme or host spelling differences
/// are genuinely different addresses and compare unequal.
fn normalize_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// Classifies a prospective re-login against the seeded session. Pure so the Welcome screen can
/// drive its confirmation dialog and `relogin` can drive the actual cleanup from one source of
/// truth — the dialog is only ever a preview of what this function says will happen.
pub fn replacement_kind(seed: &ReloginSeed, url: &str, username: &str) -> Option<ReplacementKind> {
    if normalize_url(url) == normalize_url(&seed.url) {
        if username == seed.username {
            None
        } else {
            Some(ReplacementKind::SameServerNewAccount)
        }
    } else {
        Some(ReplacementKind::NewServer)
    }
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

    let account_id = accounts::add(
        pool,
        &server_id,
        &login_result.username,
        &login_result.access_token,
        login_result.refresh_token.as_deref(),
    )
    .await?;
    accounts::set_active(pool, &account_id).await?;

    Ok(AddedAccount { server_id, account_id })
}

/// Sign in again after the previous session died (revoked/expired refresh token, removed user) or
/// the user wants a different account/server — the flow behind the "Log in again" button on
/// Home's failure state. The caller (the Welcome screen) has already confirmed any replacement
/// with the user via `replacement_kind`; this function then guarantees the invariants:
///
/// - a failed login persists nothing — the old session stays active and reachable;
/// - same URL + username (server-canonical) → only the token pair is rotated, all local data is
///   kept, ids come back unchanged;
/// - same URL, different username → the old account row is removed (its progress/bookmarks
///   cascade) but the server's cache survives, since it's keyed by `server_id`;
/// - different URL → the old server row is removed entirely (cascading its libraries, items,
///   chapters and downloads) and its on-disk cover/download files are purged.
pub async fn relogin(
    pool: &SqlitePool,
    paths: &AppPaths,
    previous: &ReloginSeed,
    url: &str,
    username: &str,
    password: &str,
) -> Result<AddedAccount> {
    // Normalized once, up front: every use below (the HTTP client's baseurl, the same-server
    // comparison, the new server row) sees the same canonical form, so a user-typed trailing
    // slash can't fork the flow.
    let url = normalize_url(url);
    // A same-server re-login honors the server's saved connection settings — a self-signed
    // server must be reachable through its own TLS policy, or "sign in again" would be the one
    // flow that breaks exactly where every other call works. Any other case (a different
    // server) has no settings to honor yet: defaults apply, and connection settings can only
    // be configured once a server exists.
    let options = match servers::get(pool, &previous.server_id).await {
        Ok(row) if normalize_url(&row.url) == url => crate::connection::ConnectionTarget::resolve(&row, None).options,
        _ => abs_api::ConnectionOptions::default(),
    };
    let client = abs_api::Client::with_options(url, &options).map_err(|e| crate::error::CoreError::UnexpectedResponse(e.to_string()))?;
    let login_result = client.login(username, password).await?;

    let same_server = url == normalize_url(&previous.url);
    let same_account = same_server && login_result.username == previous.username;

    if same_account {
        accounts::set_tokens(
            pool,
            &previous.account_id,
            &login_result.access_token,
            login_result.refresh_token.as_deref(),
        )
        .await?;
        return Ok(AddedAccount {
            server_id: previous.server_id.clone(),
            account_id: previous.account_id.clone(),
        });
    }

    if same_server {
        let account_id = accounts::add(
            pool,
            &previous.server_id,
            &login_result.username,
            &login_result.access_token,
            login_result.refresh_token.as_deref(),
        )
        .await?;
        accounts::set_active(pool, &account_id).await?;
        accounts::remove(pool, &previous.account_id).await?;
        return Ok(AddedAccount { server_id: previous.server_id.clone(), account_id });
    }

    let server_id = servers::add(pool, url).await?;
    let account_id = match accounts::add(
        pool,
        &server_id,
        &login_result.username,
        &login_result.access_token,
        login_result.refresh_token.as_deref(),
    )
    .await
    {
        Ok(account_id) => account_id,
        Err(err) => {
            // Same rollback discipline as `add_server_and_login`: no credential-less server row.
            let _ = servers::remove(pool, &server_id).await;
            return Err(err.into());
        }
    };
    accounts::set_active(pool, &account_id).await?;
    servers::remove(pool, &previous.server_id).await?;
    if let Err(err) = paths.purge_server_data(&previous.server_id).await {
        tracing::warn!(%err, server_id = %previous.server_id, "couldn't purge the old server's on-disk cache after switching servers; the files are orphaned but harmless");
    }

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
    use abs_storage::repo::items::{self, UpsertItem};
    use abs_storage::repo::libraries::{self, UpsertLibrary};
    use abs_storage::repo::progress;
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

    /// One synced library + one item + one progress row, so the tests can observe what a
    /// replacement keeps and what it throws away.
    async fn seed_cache(pool: &SqlitePool, server_id: &str, account_id: &str) {
        libraries::upsert(
            pool,
            UpsertLibrary {
                id: "lib-1",
                server_id,
                name: "Audiobooks",
                media_type: "book",
                icon: None,
                display_order: 1,
            },
        )
        .await
        .unwrap();
        items::upsert(
            pool,
            UpsertItem {
                id: "item-1",
                server_id,
                library_id: "lib-1",
                title: "Project Hail Mary",
                author: Some("Andy Weir"),
                narrator: None,
                description: None,
                duration_seconds: 11600.0,
                added_at: chrono::Utc::now(),
                series_name: None,
                genres: &[],
            },
        )
        .await
        .unwrap();
        progress::set(pool, account_id, server_id, "item-1", 42.0, false).await.unwrap();
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
        assert_eq!(stored_account.refresh_token.as_deref(), Some("refresh456"), "the refresh token must be persisted — it's what keeps the session alive past the access token's expiry");
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

    #[test]
    fn replacement_kind_classifies_the_three_cases() {
        let seed = ReloginSeed {
            server_id: "s".into(),
            account_id: "a".into(),
            url: "http://host/abs".into(),
            username: "jane".into(),
        };
        assert!(replacement_kind(&seed, "http://host/abs", "jane").is_none(), "same credentials need no replacement");
        assert!(
            replacement_kind(&seed, "http://host/abs/", "jane").is_none(),
            "a trailing slash is the same address, not a different server"
        );
        assert!(
            matches!(replacement_kind(&seed, "http://host/abs", "bob"), Some(ReplacementKind::SameServerNewAccount)),
            "a different username on the same server replaces the account, not the server"
        );
        assert!(
            matches!(replacement_kind(&seed, "http://elsewhere/abs", "jane"), Some(ReplacementKind::NewServer)),
            "a different URL is a different server"
        );
    }

    #[tokio::test]
    async fn relogin_with_the_same_credentials_rotates_tokens_and_keeps_everything() {
        let server = MockServer::start().await;
        // `up_to_n_times(1)` — wiremock matches first-mounted first, so without this the seeding
        // mock would also answer the re-login and the rotation below would never be observed.
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(mock_login_success())
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();
        seed_cache(&pool, &added.server_id, &added.account_id).await;
        let previous = ReloginSeed {
            server_id: added.server_id.clone(),
            account_id: added.account_id.clone(),
            url: server.uri(),
            username: "jane".into(),
        };
        // Fresh tokens on the re-login (the old pair is what died), and a trailing slash in the
        // URL to prove normalization doesn't send this down the new-server path.
        Mock::given(method("POST")).and(path("/login")).respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({
                "user": { "id": "user-1", "username": "jane", "accessToken": "new-access", "refreshToken": "new-refresh" },
            }),
        )).mount(&server).await;

        let relogged = relogin(&pool, &AppPaths::rooted_at("/tmp/unused", "/tmp/unused"), &previous, &format!("{}/", server.uri()), "jane", "hunter2")
            .await
            .unwrap();

        assert_eq!(relogged.server_id, added.server_id, "same server, so the row must be reused, not duplicated");
        assert_eq!(relogged.account_id, added.account_id, "same account, so the row must be reused, not duplicated");
        let stored = accounts::get(&pool, &added.account_id).await.unwrap();
        assert_eq!(stored.token, "new-access");
        assert_eq!(stored.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(servers::list(&pool).await.unwrap().len(), 1, "no duplicate server row");
        assert_eq!(accounts::list_for_server(&pool, &added.server_id).await.unwrap().len(), 1, "no duplicate account row");
        assert_eq!(
            libraries::list_for_server(&pool, &added.server_id).await.unwrap().len(),
            1,
            "the cached library must survive a same-account re-login untouched"
        );
    }

    #[tokio::test]
    async fn relogin_to_a_different_account_on_the_same_server_keeps_the_cache_but_replaces_the_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).up_to_n_times(1).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();
        seed_cache(&pool, &added.server_id, &added.account_id).await;
        let previous = ReloginSeed {
            server_id: added.server_id.clone(),
            account_id: added.account_id.clone(),
            url: server.uri(),
            username: "jane".into(),
        };
        Mock::given(method("POST")).and(path("/login")).respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({
                "user": { "id": "user-2", "username": "bob", "accessToken": "bob-token", "refreshToken": "bob-refresh" },
            }),
        )).mount(&server).await;

        let relogged = relogin(&pool, &AppPaths::rooted_at("/tmp/unused", "/tmp/unused"), &previous, &server.uri(), "bob", "hunter2")
            .await
            .unwrap();

        assert_eq!(relogged.server_id, added.server_id, "the server row is reused — its cache is still valid");
        assert_ne!(relogged.account_id, added.account_id);
        assert!(accounts::get(&pool, &added.account_id).await.is_err(), "the old account must go");
        assert_eq!(accounts::get(&pool, &relogged.account_id).await.unwrap().username, "bob");
        assert!(accounts::get(&pool, &relogged.account_id).await.unwrap().is_active);
        assert_eq!(
            libraries::list_for_server(&pool, &added.server_id).await.unwrap().len(),
            1,
            "libraries are keyed by server and must survive an account switch"
        );
        assert!(
            progress::get(&pool, &added.account_id, &added.server_id, "item-1").await.unwrap().is_none(),
            "the old account's local progress cascades away with its row"
        );
        assert_eq!(servers::list(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn relogin_to_a_different_server_removes_the_old_server_rows_and_files() {
        // Two distinct mock servers: the seed session lives on A, the re-login points at B.
        let server_a = MockServer::start().await;
        let server_b = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).mount(&server_a).await;
        Mock::given(method("POST")).and(path("/login")).respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({
                "user": { "id": "user-2", "username": "bob", "accessToken": "bob-token", "refreshToken": None::<String> },
            }),
        )).mount(&server_b).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server_a.uri(), "jane", "hunter2").await.unwrap();
        seed_cache(&pool, &added.server_id, &added.account_id).await;
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
        let cover = paths.cover_cache_path(&added.server_id, "item-1", "jpg");
        tokio::fs::create_dir_all(cover.parent().unwrap()).await.unwrap();
        tokio::fs::write(&cover, b"bytes").await.unwrap();

        // The username is deliberately the same as the seed's — the URL alone must drive the
        // server switch.
        let previous = ReloginSeed {
            server_id: added.server_id.clone(),
            account_id: added.account_id.clone(),
            url: server_a.uri(),
            username: "jane".into(),
        };
        let relogged = relogin(&pool, &paths, &previous, &server_b.uri(), "bob", "hunter2").await.unwrap();

        assert_ne!(relogged.server_id, added.server_id);
        assert!(servers::get(&pool, &added.server_id).await.is_err(), "the old server row must go");
        assert!(accounts::get(&pool, &added.account_id).await.is_err(), "the old account must go with it");
        assert!(
            libraries::list_for_server(&pool, &added.server_id).await.unwrap().is_empty(),
            "the old server's cached rows cascade away"
        );
        assert_eq!(accounts::get(&pool, &relogged.account_id).await.unwrap().username, "bob");
        assert!(accounts::get(&pool, &relogged.account_id).await.unwrap().is_active);
        assert!(!cover.exists(), "the old server's on-disk covers must be purged, not orphaned");
    }

    #[tokio::test]
    async fn relogin_failure_leaves_the_old_session_intact() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/login")).respond_with(mock_login_success()).up_to_n_times(1).mount(&server).await;

        let pool = pool().await;
        let added = add_server_and_login(&pool, &server.uri(), "jane", "hunter2").await.unwrap();
        let previous = ReloginSeed {
            server_id: added.server_id.clone(),
            account_id: added.account_id.clone(),
            url: server.uri(),
            username: "jane".into(),
        };
        Mock::given(method("POST")).and(path("/login")).respond_with(ResponseTemplate::new(401)).mount(&server).await;

        let result = relogin(&pool, &AppPaths::rooted_at("/tmp/unused", "/tmp/unused"), &previous, &server.uri(), "jane", "wrong").await;

        assert!(matches!(result, Err(CoreError::Login(_))));
        assert!(accounts::get(&pool, &added.account_id).await.is_ok(), "the old account must survive a failed re-login");
        assert!(accounts::get(&pool, &added.account_id).await.unwrap().is_active, "and must still be the active one");
        assert_eq!(servers::list(&pool).await.unwrap().len(), 1, "no new server row from a failed re-login");
    }
}
