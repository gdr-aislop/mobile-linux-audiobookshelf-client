//! Remembered passwords: what lets a session sign in again on its own when the server has ended
//! it (see [`crate::auth`]). The store itself is the app's (the desktop keyring), handed in through
//! [`PasswordStore`] the same way [`crate::auth::KeepAwake`] is; this crate never names it.
//! Passwords are kept per account, by the account's local id.

use futures::future::BoxFuture;

/// Where remembered passwords live. Every method is best-effort: an error is a message to log,
/// never a reason to fail the login or sign-out it accompanies.
pub trait PasswordStore: Send + Sync {
    fn save<'a>(&'a self, account_id: &'a str, password: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// The account's password, `None` when none is remembered.
    fn load<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<Option<String>, String>>;
    fn forget<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

/// A store that remembers nothing — for callers with no keyring to offer.
pub struct NoPasswords;

impl PasswordStore for NoPasswords {
    fn save<'a>(&'a self, _account_id: &'a str, _password: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn load<'a>(&'a self, _account_id: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn forget<'a>(&'a self, _account_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Saves or forgets an account's password after a successful login, as the user chose. Failures
/// are logged: the login already succeeded and stands either way.
pub async fn remember_after_login(store: &dyn PasswordStore, account_id: &str, password: &str, remember: bool) {
    let result = if remember { store.save(account_id, password).await } else { store.forget(account_id).await };
    match result {
        Ok(()) if remember => tracing::info!(account_id, "remembered the password in the keyring"),
        Ok(()) => tracing::info!(account_id, "not remembering the password; any remembered one was removed"),
        // Loud on purpose: without it the app can't sign in again by itself, and nothing on screen
        // says so — this line is how a missing or locked keyring shows up.
        Err(err) => tracing::warn!(
            %err,
            account_id,
            remember,
            "couldn't update the remembered password in the keyring; signing in again by itself won't work"
        ),
    }
}

/// Forgets an account's password because the account is gone. Failures are logged.
pub async fn forget(store: &dyn PasswordStore, account_id: &str) {
    if let Err(err) = store.forget(account_id).await {
        tracing::warn!(%err, account_id, "couldn't forget the remembered password");
    }
}

/// An in-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryPasswords(pub(crate) std::sync::Mutex<std::collections::HashMap<String, String>>);

#[cfg(test)]
impl MemoryPasswords {
    pub(crate) fn with(account_id: &str, password: &str) -> Self {
        let store = Self::default();
        store.0.lock().unwrap().insert(account_id.to_string(), password.to_string());
        store
    }

    pub(crate) fn get(&self, account_id: &str) -> Option<String> {
        self.0.lock().unwrap().get(account_id).cloned()
    }
}

#[cfg(test)]
impl PasswordStore for MemoryPasswords {
    fn save<'a>(&'a self, account_id: &'a str, password: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.0.lock().unwrap().insert(account_id.to_string(), password.to_string());
        Box::pin(async { Ok(()) })
    }

    fn load<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        let password = self.get(account_id);
        Box::pin(async move { Ok(password) })
    }

    fn forget<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.0.lock().unwrap().remove(account_id);
        Box::pin(async { Ok(()) })
    }
}
