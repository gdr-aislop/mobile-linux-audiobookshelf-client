//! Keeps a stored account's access token working across launches and across a long session.
//! Audiobookshelf servers from v2.26.0 on issue short-lived JWT access tokens (1–12 hours
//! depending on version/settings) paired with a long-lived refresh token that **rotates on every
//! use** — so a client like this one, which persists tokens across runs and can sit open for
//! hours, must: notice an expiring access token, exchange the refresh token at
//! `POST /auth/refresh`, and persist the rotated pair. Skipping this is exactly the "logged out
//! in ~an hour" behavior this module exists to prevent (observed live: login at ~18:05, every
//! API call 401 by 19:03).
//!
//! Layering: [`Session`] lives in `abs-core` because it is domain logic (DB + HTTP, no widgets);
//! it depends only on `abs-storage` and `abs-api`. The `app` crate constructs one per login and
//! hands it to screens, which ask it for a token at *call* time instead of capturing
//! `account.token` once at build time — the capture-once pattern is what let tokens die
//! mid-session. Everything is deliberately best-effort in the same posture as
//! `covers`/`progress_sync`: if a refresh can't be attempted (legacy server, no refresh token)
//! or fails (server down, session revoked), the current token is returned and the failure is
//! only logged — the next 401 is no worse than what would happen without this module.

use std::sync::Arc;

use sqlx::SqlitePool;

use abs_storage::models::Account;

/// A JWT is only "fresh enough" if it stays valid through this margin — a token 30 seconds from
/// expiry would otherwise still race the server's clock by the time a request lands.
const FRESH_MARGIN_SECONDS: i64 = 60;

/// The access token's `exp` claim, in Unix seconds. `None` when this isn't an expiring JWT at all
/// (legacy servers' permanent tokens have no such claim — those need no refreshing), or the
/// payload doesn't parse, which this treats the same way: "can't tell, don't touch it".
pub fn jwt_exp_seconds(token: &str) -> Option<i64> {
    use base64::Engine;

    let payload = token.split('.').nth(1)?;
    let payload = payload.trim_end_matches('=');
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json.get("exp")?.as_i64()
}

/// One logged-in session's live token pair: seeded from the persisted [`Account`], refreshed
/// (and persisted back) whenever a caller asks for a token that is expired or about to be.
/// Cheap to clone — every holder sees the same refreshed pair, and a tokio mutex makes
/// concurrent callers share one refresh rather than stampeding the server.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    pool: SqlitePool,
    server_url: String,
    server_id: String,
    account_id: String,
    tokens: tokio::sync::Mutex<Tokens>,
}

#[derive(Clone)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
}

impl Session {
    pub fn new(pool: SqlitePool, server_url: &str, server_id: &str, account: &Account) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                pool,
                server_url: server_url.to_string(),
                server_id: server_id.to_string(),
                account_id: account.id.clone(),
                tokens: tokio::sync::Mutex::new(Tokens {
                    access_token: account.token.clone(),
                    refresh_token: account.refresh_token.clone(),
                }),
            }),
        }
    }

    pub fn account_id(&self) -> &str {
        &self.inner.account_id
    }

    pub fn server_id(&self) -> &str {
        &self.inner.server_id
    }

    pub fn server_url(&self) -> &str {
        &self.inner.server_url
    }

    /// A current access token, refreshing first when the stored one is expired or within
    /// [`FRESH_MARGIN_SECONDS`] of it. Infallible by design: on a failed refresh the existing
    /// token comes back (the caller's request will surface the 401, if any, as its own error)
    /// and the failure is logged. Holding the tokens lock across the refresh makes concurrent
    /// callers wait for — and then reuse — one shared refresh instead of racing several.
    pub async fn access_token(&self) -> String {
        let mut tokens = self.inner.tokens.lock().await;
        let fresh = match jwt_exp_seconds(&tokens.access_token) {
            None => true,
            Some(exp) => exp > chrono::Utc::now().timestamp() + FRESH_MARGIN_SECONDS,
        };
        if fresh {
            return tokens.access_token.clone();
        }

        let refresh_token = match &tokens.refresh_token {
            Some(refresh_token) => refresh_token.clone(),
            None => {
                // Legacy server (permanent tokens don't carry `exp`) or an account that never
                // had one: nothing to exchange.
                return tokens.access_token.clone();
            }
        };

        let client = abs_api::Client::new(&self.inner.server_url);
        match client.refresh(&refresh_token).await {
            Ok(result) => {
                // Rotation: the response's refresh token replaces the old one. Servers that
                // don't rotate (or pre-date rotation semantics) may omit it — keeping the
                // existing token is the safe fallback there, and is also what the server's own
                // grace window (v2.35.0+) tolerates.
                let new_refresh = result.refresh_token.or_else(|| tokens.refresh_token.clone());
                if let Err(err) = abs_storage::repo::accounts::set_tokens(
                    &self.inner.pool,
                    &self.inner.account_id,
                    &result.access_token,
                    new_refresh.as_deref(),
                )
                .await
                {
                    tracing::warn!(%err, "refreshed the access token but couldn't persist it; it will be re-refreshed next launch");
                }
                tracing::info!(account_id = %self.inner.account_id, "refreshed the account's access token");
                tokens.access_token = result.access_token;
                tokens.refresh_token = new_refresh;
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    account_id = %self.inner.account_id,
                    "couldn't refresh the access token; the stored token will be used and may be rejected"
                );
            }
        }

        tokens.access_token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A JWT-shaped token (`header.payload.signature`) whose payload carries the given `exp`.
    /// Signature is fake — this client never verifies signatures (only the server does).
    fn jwt_with_exp(exp: i64) -> String {
        let payload = serde_json::json!({ "sub": "user-1", "exp": exp });
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        format!(
            "{}.{}.{}",
            encode(br#"{"alg":"HS256"}"#),
            encode(payload.to_string().as_bytes()),
            encode(b"signature")
        )
    }

    async fn pool_with_account(token: &str, refresh_token: Option<&str>) -> (SqlitePool, Account, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = abs_storage::repo::servers::add(&pool, "https://example.invalid").await.unwrap();
        let account_id = abs_storage::repo::accounts::add(&pool, &server_id, "jane", token, refresh_token).await.unwrap();
        let account = abs_storage::repo::accounts::get(&pool, &account_id).await.unwrap();
        (pool, account, server_id)
    }

    fn session_for(mock_url: &str, pool: &SqlitePool, account: &Account) -> Session {
        Session::new(pool.clone(), mock_url, &account.server_id, account)
    }

    #[test]
    fn jwt_exp_seconds_decodes_the_exp_claim() {
        let exp = chrono::Utc::now().timestamp() + 1234;
        assert_eq!(jwt_exp_seconds(&jwt_with_exp(exp)), Some(exp));
    }

    #[test]
    fn jwt_exp_seconds_is_none_for_non_jwt_tokens() {
        assert_eq!(jwt_exp_seconds("plain-legacy-token"), None);
        assert_eq!(jwt_exp_seconds("a.b"), None, "payload that isn't JSON");
        assert_eq!(jwt_exp_seconds(""), None);
    }

    #[tokio::test]
    async fn a_fresh_token_is_returned_unchanged_without_any_network_call() {
        let exp = chrono::Utc::now().timestamp() + 3600;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(exp), Some("refresh")).await;
        let mock_server = MockServer::start().await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let token = session.access_token().await;

        assert_eq!(token, account.token);
        assert!(mock_server.received_requests().await.unwrap().is_empty(), "no refresh request should be sent");
    }

    #[tokio::test]
    async fn an_expiring_token_is_refreshed_and_both_rotated_tokens_persisted() {
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), Some("old-refresh")).await;
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/refresh"))
            .and(header("x-refresh-token", "old-refresh"))
            .and(header("x-return-tokens", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": "user-1",
                    "username": "jane",
                    "accessToken": "new-access",
                    "refreshToken": "new-refresh",
                }
            })))
            .mount(&mock_server)
            .await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let token = session.access_token().await;

        assert_eq!(token, "new-access");
        let stored = abs_storage::repo::accounts::get(&pool, &account.id).await.unwrap();
        assert_eq!(stored.token, "new-access", "the fresh pair must be persisted, not just returned");
        assert_eq!(stored.refresh_token.as_deref(), Some("new-refresh"), "rotation must be picked up");
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), Some("old-refresh")).await;
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": { "id": "user-1", "username": "jane", "accessToken": "new-access", "refreshToken": "new-refresh" }
            })))
            .mount(&mock_server)
            .await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let fetched = {
            let (a, b, c, d, e) = tokio::join!(session.access_token(), session.access_token(), session.access_token(), session.access_token(), session.access_token());
            vec![a, b, c, d, e]
        };

        assert!(fetched.iter().all(|t| t == "new-access"), "all callers get the refreshed token: {fetched:?}");
        assert_eq!(
            mock_server.received_requests().await.unwrap().len(),
            1,
            "one shared refresh, not one per caller (a refresh token rotates on use — racing refreshes would strand all but one caller)"
        );
    }

    #[tokio::test]
    async fn a_refresh_response_without_a_new_refresh_token_keeps_the_old_one() {
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), Some("old-refresh")).await;
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": { "id": "user-1", "username": "jane", "accessToken": "new-access" }
            })))
            .mount(&mock_server)
            .await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let token = session.access_token().await;

        assert_eq!(token, "new-access");
        let stored = abs_storage::repo::accounts::get(&pool, &account.id).await.unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("old-refresh"));
    }

    #[tokio::test]
    async fn a_failed_refresh_returns_the_current_token_gracefully() {
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), Some("revoked-refresh")).await;
        let mock_server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/auth/refresh")).respond_with(ResponseTemplate::new(401)).mount(&mock_server).await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let token = session.access_token().await;

        assert_eq!(token, account.token, "the stored token is still what the caller uses");
        let stored = abs_storage::repo::accounts::get(&pool, &account.id).await.unwrap();
        assert_eq!(stored.token, account.token, "nothing is overwritten on a failed refresh");
    }

    #[tokio::test]
    async fn an_expired_token_with_no_refresh_token_is_left_alone() {
        // Legacy server case: no refresh token to exchange — the call must not hit the network.
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), None).await;
        let mock_server = MockServer::start().await;
        let session = session_for(&mock_server.uri(), &pool, &account);

        let token = session.access_token().await;

        assert_eq!(token, account.token);
        assert!(mock_server.received_requests().await.unwrap().is_empty());
    }
}
