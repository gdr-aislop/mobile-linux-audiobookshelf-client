//! The app's [`abs_core::passwords::PasswordStore`]: remembered passwords kept in the desktop
//! keyring through `oo7` (the Secret Service — GNOME Keyring on Phosh — on the host, the Secret
//! portal inside Flatpak). A locked keyring is unlocked on first use, which can show the system's
//! unlock prompt. With no keyring at all, every call fails and nothing is remembered: passwords are
//! never written anywhere else.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;

/// Identifies this app's items among everything else in the keyring.
const SCHEMA: &str = "io.github.gdr_aislop.abs-app.Password";

#[derive(Default)]
pub(crate) struct KeyringPasswordStore {
    /// Opened (and unlocked) on first use, then shared by every call.
    keyring: tokio::sync::OnceCell<oo7::Keyring>,
}

/// The one store the whole app shares, so every screen sees the same remembered passwords. Tests
/// get one that remembers nothing: they run without a keyring, and must never reach the user's.
pub(crate) fn store() -> Arc<dyn abs_core::passwords::PasswordStore> {
    static STORE: OnceLock<Arc<dyn abs_core::passwords::PasswordStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            if cfg!(test) {
                Arc::new(abs_core::passwords::NoPasswords)
            } else {
                Arc::new(KeyringPasswordStore::default())
            }
        })
        .clone()
}

fn attributes(account_id: &str) -> HashMap<&str, &str> {
    HashMap::from([("xdg:schema", SCHEMA), ("account-id", account_id)])
}

impl KeyringPasswordStore {
    async fn keyring(&self) -> Result<&oo7::Keyring, String> {
        self.keyring
            .get_or_try_init(|| async {
                let keyring = oo7::Keyring::new().await.map_err(|err| format!("couldn't open the keyring: {err}"))?;
                keyring.unlock().await.map_err(|err| format!("couldn't unlock the keyring: {err}"))?;
                Ok(keyring)
            })
            .await
    }
}

impl abs_core::passwords::PasswordStore for KeyringPasswordStore {
    fn save<'a>(&'a self, account_id: &'a str, password: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.keyring()
                .await?
                .create_item("Audiobookshelf account password", &attributes(account_id), password, true)
                .await
                .map_err(|err| format!("couldn't save the password: {err}"))
        })
    }

    fn load<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            let items = self.keyring().await?.search_items(&attributes(account_id)).await.map_err(|err| format!("couldn't search the keyring: {err}"))?;
            let Some(item) = items.first() else { return Ok(None) };
            if item.is_locked().await.unwrap_or(false) {
                item.unlock().await.map_err(|err| format!("couldn't unlock the password: {err}"))?;
            }
            let secret = item.secret().await.map_err(|err| format!("couldn't read the password: {err}"))?;
            String::from_utf8(secret.as_bytes().to_vec()).map(Some).map_err(|_| "the stored password isn't text".to_string())
        })
    }

    fn forget<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.keyring()
                .await?
                .delete(&attributes(account_id))
                .await
                .map_err(|err| format!("couldn't remove the password: {err}"))
        })
    }
}
