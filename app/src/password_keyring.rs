//! The app's [`abs_core::passwords::PasswordStore`]: remembered passwords kept in the desktop
//! keyring through `oo7` (the Secret Service — GNOME Keyring on Phosh — on the host, the Secret
//! portal inside Flatpak). A locked keyring is unlocked on first use, which can show the system's
//! unlock prompt. With no keyring at all, every call fails and nothing is remembered: passwords are
//! never written anywhere else.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;

/// Identifies this app's items among everything else in the keyring.
const SCHEMA: &str = "io.github.gdr_aislop.audiobooklet.Password";

/// The schema up to 0.9.5, when the app was called abs-app. A password found only under it is
/// moved to [`SCHEMA`] the first time it's read.
const LEGACY_SCHEMA: &str = "io.github.gdr_aislop.abs-app.Password";

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
    attributes_for(SCHEMA, account_id)
}

fn attributes_for<'a>(schema: &'a str, account_id: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("xdg:schema", schema), ("account-id", account_id)])
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
                .create_item(&crate::i18n::tr("Audiobooklet — server password"), &attributes(account_id), password, true)
                .await
                .map_err(|err| format!("couldn't save the password: {err}"))
        })
    }

    fn load<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            let keyring = self.keyring().await?;
            if let Some(password) = read_password(keyring, &attributes(account_id)).await? {
                return Ok(Some(password));
            }
            // Saved before the rename: move it under the current schema, then drop the old item.
            let legacy = attributes_for(LEGACY_SCHEMA, account_id);
            let Some(password) = read_password(keyring, &legacy).await? else { return Ok(None) };
            match self.save(account_id, &password).await {
                Ok(()) => {
                    if let Err(err) = keyring.delete(&legacy).await {
                        tracing::warn!(%err, account_id, "moved the remembered password to the new name but couldn't remove the old entry");
                    } else {
                        tracing::info!(account_id, "moved the remembered password to the app's new name");
                    }
                }
                Err(err) => tracing::warn!(%err, account_id, "couldn't move the remembered password to the app's new name; using the old entry"),
            }
            Ok(Some(password))
        })
    }

    fn forget<'a>(&'a self, account_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let keyring = self.keyring().await?;
            for schema in [SCHEMA, LEGACY_SCHEMA] {
                keyring
                    .delete(&attributes_for(schema, account_id))
                    .await
                    .map_err(|err| format!("couldn't remove the password: {err}"))?;
            }
            Ok(())
        })
    }
}

/// The secret of the first item matching `attributes`, unlocking it if needed.
async fn read_password(keyring: &oo7::Keyring, attributes: &HashMap<&str, &str>) -> Result<Option<String>, String> {
    let items = keyring.search_items(attributes).await.map_err(|err| format!("couldn't search the keyring: {err}"))?;
    let Some(item) = items.first() else { return Ok(None) };
    if item.is_locked().await.unwrap_or(false) {
        item.unlock().await.map_err(|err| format!("couldn't unlock the password: {err}"))?;
    }
    let secret = item.secret().await.map_err(|err| format!("couldn't read the password: {err}"))?;
    String::from_utf8(secret.as_bytes().to_vec()).map(Some).map_err(|_| "the stored password isn't text".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abs_core::passwords::PasswordStore;

    /// The real keyring path, end to end: save, replace, load and forget through `oo7` against
    /// whatever Secret Service is on the session bus. Not run by default — it needs one (GNOME
    /// Keyring, as on Phosh) and must never touch a developer's own keyring by accident:
    ///
    ///     dbus-run-session -- sh -c 'echo test | gnome-keyring-daemon --unlock --components=secrets >/dev/null &&
    ///       cargo test -p audiobooklet -- --exact password_keyring::tests::round_trips_a_password_through_the_desktop_keyring --ignored'
    #[test]
    #[ignore = "needs a Secret Service (gnome-keyring) on the session bus; run by hand, see the doc comment"]
    fn round_trips_a_password_through_the_desktop_keyring() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let store = KeyringPasswordStore::default();
            let account_id = format!("keyring-test-{}", std::process::id());
            assert_eq!(store.load(&account_id).await.unwrap(), None, "nothing stored yet");
            store.save(&account_id, "hunter2").await.unwrap();
            assert_eq!(store.load(&account_id).await.unwrap().as_deref(), Some("hunter2"));
            store.save(&account_id, "zażółć gęślą jaźń").await.unwrap();
            assert_eq!(
                store.load(&account_id).await.unwrap().as_deref(),
                Some("zażółć gęślą jaźń"),
                "saving again replaces the password (and non-ASCII survives the round trip)"
            );
            store.forget(&account_id).await.unwrap();
            assert_eq!(store.load(&account_id).await.unwrap(), None, "forgotten");

            // A password saved under the old name's schema is found, and moved to the new one.
            let keyring = store.keyring().await.unwrap();
            keyring
                .create_item("legacy", &attributes_for(LEGACY_SCHEMA, &account_id), "from-abs-app", true)
                .await
                .unwrap();
            assert_eq!(store.load(&account_id).await.unwrap().as_deref(), Some("from-abs-app"));
            assert!(keyring.search_items(&attributes_for(LEGACY_SCHEMA, &account_id)).await.unwrap().is_empty(), "the old entry is gone");
            assert_eq!(read_password(keyring, &attributes(&account_id)).await.unwrap().as_deref(), Some("from-abs-app"), "now under the new schema");
            store.forget(&account_id).await.unwrap();
            assert_eq!(store.load(&account_id).await.unwrap(), None);
        });
    }
}
