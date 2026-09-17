//! Keeps a stored account's access token working across launches. Audiobookshelf servers from
//! v2.26.0 on issue short-lived JWT access tokens (1–12 hours depending on version/settings)
//! paired with a long-lived refresh token that **rotates on every use** — so a client like this
//! one, which persists tokens across runs, must: notice an expiring access token, exchange the
//! refresh token at `POST /auth/refresh`, and persist the rotated pair. Skipping this is exactly
//! the "logged out in ~an hour" behavior this module exists to prevent (observed live: login at
//! ~18:05, every API call 401 by 19:03).
//!
//! Everything here is deliberately best-effort in the same posture as `covers`/`progress_sync`:
//! if a refresh can't be attempted (legacy server, no refresh token) or fails (server down,
//! session revoked), the stored account is returned unchanged and the caller proceeds with the
//! old token — the next 401 is no worse than what would happen without this module.

use sqlx::SqlitePool;

use abs_storage::models::Account;

use crate::error::Result;

/// A JWT is only "fresh enough" if it stays valid through this margin — a run that starts with
/// the token 30 seconds from expiry would otherwise still race the server's clock mid-session.
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

async fn refresh_and_persist(pool: &SqlitePool, baseurl: &str, account: &Account) -> Result<Account> {
    let refresh_token = account
        .refresh_token
        .as_deref()
        .ok_or_else(|| crate::CoreError::Login(abs_api::LoginError::SessionExpired))?;

    let client = abs_api::Client::new(baseurl);
    let refreshed = client.refresh(refresh_token).await?;

    // Rotation: the response's refresh token replaces the old one. Servers that don't rotate
    // (or pre-date rotation semantics) may omit it — keeping the existing token is the safe
    // fallback there, and is also what the server's own grace window (v2.35.0+) tolerates.
    let new_refresh = refreshed.refresh_token.as_deref().or(account.refresh_token.as_deref());
    abs_storage::repo::accounts::set_tokens(pool, &account.id, &refreshed.access_token, new_refresh).await?;

    Ok(Account {
        token: refreshed.access_token,
        refresh_token: new_refresh.map(str::to_string),
        ..account.clone()
    })
}

/// Returns `account` with a fresh access token: unchanged when the token doesn't expire soon
/// (or doesn't expire at all), refreshed-and-persisted when it does. Never fails the caller —
/// on any error the original account comes back and the failure is only logged.
pub async fn ensure_fresh_token(pool: &SqlitePool, baseurl: &str, account: &Account) -> Account {
    let exp = jwt_exp_seconds(&account.token);
    let fresh = match exp {
        // No expiry claim (legacy token) → nothing to do. Also treat absurdly-old-but-unexpired
        // claims uniformly: the comparison below is all that matters.
        None => true,
        Some(exp) => exp > chrono::Utc::now().timestamp() + FRESH_MARGIN_SECONDS,
    };
    if fresh {
        return account.clone();
    }

    match refresh_and_persist(pool, baseurl, account).await {
        Ok(refreshed) => {
            tracing::info!(account_id = %account.id, "refreshed the account's access token");
            refreshed
        }
        Err(err) => {
            tracing::warn!(
                %err,
                account_id = %account.id,
                "couldn't refresh the access token; the stored token will be used and may be rejected"
            );
            account.clone()
        }
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

        let result = ensure_fresh_token(&pool, &mock_server.uri(), &account).await;

        assert_eq!(result.token, account.token);
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

        let result = ensure_fresh_token(&pool, &mock_server.uri(), &account).await;

        assert_eq!(result.token, "new-access");
        assert_eq!(result.refresh_token.as_deref(), Some("new-refresh"), "rotation must be picked up");
        let stored = abs_storage::repo::accounts::get(&pool, &account.id).await.unwrap();
        assert_eq!(stored.token, "new-access", "the fresh pair must be persisted, not just returned");
        assert_eq!(stored.refresh_token.as_deref(), Some("new-refresh"));
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

        let result = ensure_fresh_token(&pool, &mock_server.uri(), &account).await;

        assert_eq!(result.token, "new-access");
        assert_eq!(result.refresh_token.as_deref(), Some("old-refresh"));
    }

    #[tokio::test]
    async fn a_failed_refresh_returns_the_original_account_gracefully() {
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), Some("revoked-refresh")).await;
        let mock_server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/auth/refresh")).respond_with(ResponseTemplate::new(401)).mount(&mock_server).await;

        let result = ensure_fresh_token(&pool, &mock_server.uri(), &account).await;

        assert_eq!(result.token, account.token, "the stored token is still what the caller uses");
        assert_eq!(result.refresh_token.as_deref(), Some("revoked-refresh"));
        let stored = abs_storage::repo::accounts::get(&pool, &account.id).await.unwrap();
        assert_eq!(stored.token, account.token, "nothing is overwritten on a failed refresh");
    }

    #[tokio::test]
    async fn an_expired_token_with_no_refresh_token_is_left_alone() {
        // Legacy server case: no refresh token to exchange — the call must not hit the network.
        let expired = chrono::Utc::now().timestamp() - 10;
        let (pool, account, _) = pool_with_account(&jwt_with_exp(expired), None).await;
        let mock_server = MockServer::start().await;

        let result = ensure_fresh_token(&pool, &mock_server.uri(), &account).await;

        assert_eq!(result.token, account.token);
        assert!(mock_server.received_requests().await.unwrap().is_empty());
    }
}
