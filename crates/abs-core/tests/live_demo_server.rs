//! Verifies `add_server_and_login` against a real, live Audiobookshelf instance rather than only
//! `wiremock` — the public demo server at https://audiobooks.dev/audiobookshelf (username and
//! password both `demo`). Confirmed reachable and running server 2.36.0 (matching what the
//! vendored spec and `abs_api::Client::login` were built against) via `GET
//! /audiobookshelf/status` before writing this test.
//!
//! Note: this server issues a unique per-session username on login (e.g. `demo467087`, not
//! literally `demo`) — discovered by actually running this test, presumably so each concurrent
//! demo visitor gets an isolated guest account. The assertions below account for that.
//!
//! `#[ignore]`d so `cargo test --workspace` stays hermetic — this depends on outside network and
//! a specific external service staying up. Run explicitly with:
//!   cargo test -p abs-core --test live_demo_server -- --ignored --nocapture

use abs_storage::repo::{accounts, servers};

const DEMO_SERVER_URL: &str = "https://audiobooks.dev/audiobookshelf";

#[tokio::test]
#[ignore]
async fn live_login_against_demo_server_succeeds() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3"))
        .await
        .expect("connect and migrate temp database");

    let added = abs_core::accounts::add_server_and_login(&pool, DEMO_SERVER_URL, "demo", "demo")
        .await
        .expect("login against the live demo server should succeed");

    let stored_server = servers::get(&pool, &added.server_id).await.unwrap();
    assert_eq!(stored_server.url, DEMO_SERVER_URL);

    let stored_account = accounts::get(&pool, &added.account_id).await.unwrap();
    // This public demo server issues a unique per-session username (e.g. "demo467087") rather
    // than always "demo" literally — discovered by actually running this test, not assumed —
    // presumably to give each concurrent demo visitor an isolated guest account.
    assert!(
        stored_account.username.starts_with("demo"),
        "expected a demo-prefixed username, got {:?}",
        stored_account.username
    );
    assert!(
        !stored_account.token.is_empty(),
        "a real access token should have been persisted"
    );

    println!(
        "live login OK: server_id={} account_id={} token_len={}",
        added.server_id,
        added.account_id,
        stored_account.token.len()
    );
}

#[tokio::test]
#[ignore]
async fn live_login_against_demo_server_with_wrong_password_fails_cleanly() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3"))
        .await
        .expect("connect and migrate temp database");

    let result =
        abs_core::accounts::add_server_and_login(&pool, DEMO_SERVER_URL, "demo", "definitely-wrong")
            .await;

    assert!(result.is_err(), "wrong credentials must not succeed");
    assert!(
        servers::list(&pool).await.unwrap().is_empty(),
        "no server row should remain after a failed live login"
    );
}
