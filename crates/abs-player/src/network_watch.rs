//! Metered-connection detection for the "Wi-Fi-only downloads" setting — `docs/design/ui-spec.md`
//! doesn't describe a dedicated UI for this beyond the persisted setting itself
//! (`abs_core::settings::PlaybackSettings::wifi_only_downloads`), but honoring it for real needs
//! to know whether the active network connection is one a user would consider "data" (cellular, a
//! metered hotspot) versus effectively free (a home Wi-Fi network). Lives in `abs-player` for the
//! same reason `mpris`/`call_watch` do — real OS/D-Bus integration, not a widget-toolkit concern —
//! even though this isn't strictly audio-playback-related; it's the one place in this workspace
//! that already owns D-Bus plumbing via `gio`.
//!
//! Checks NetworkManager's own `Metered` property (`org.freedesktop.NetworkManager`) rather than
//! trying to enumerate connections and guess a device type from them — `Metered` is exactly the
//! semantic question a "should this cost the user money" check needs, and it already accounts for
//! things a device-type guess wouldn't (a Wi-Fi network the user has explicitly marked metered, a
//! phone acting as a mobile hotspot detected automatically, ...).
//!
//! Deliberately pull-based (checked once per download attempt, not subscribed to
//! `PropertiesChanged`): a live subscription that aborts an in-flight transfer the instant the
//! network type flips is a real future enhancement, but out of scope for a first correct pass —
//! see `abs_core::download_tracks`'s module doc for where this gets called from.

const NM_BUS_NAME: &str = "org.freedesktop.NetworkManager";
const NM_OBJECT_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// NetworkManager's `NMMetered` enum values (`org.freedesktop.NetworkManager` D-Bus API
/// reference). `Unknown` deliberately doesn't get its own match arm below — anything not
/// recognized here falls back to "undeterminable", the same as `Unknown` itself.
const NM_METERED_YES: u32 = 1;
const NM_METERED_NO: u32 = 2;
const NM_METERED_GUESS_YES: u32 = 3;
const NM_METERED_GUESS_NO: u32 = 4;

/// Whether the active network connection is metered. Implemented by [`NetworkManagerMonitor`] for
/// real systems and a `FakeNetworkMonitor` (test-only) for dependency injection, same pattern as
/// [`crate::call_watch::CallWatcher`].
pub trait NetworkMonitor {
    /// `None` means "couldn't determine" (no bus, no NetworkManager, an unrecognized value) —
    /// callers must treat this the same as "not metered" rather than blocking a feature on an
    /// answer that was never available.
    fn is_metered(&self) -> Option<bool>;
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkWatchError {
    #[error("couldn't reach the D-Bus system bus: {0}")]
    NoSystemBus(glib::Error),
}

/// Queries NetworkManager's system-bus interface. Connecting to the system bus can fail in a
/// sandbox or container with none at all; `new()` returns `Err` rather than panicking, matching
/// this crate's existing tolerance (`GstBackend::new()`, `mpris::register`, `ModemManagerCallWatcher::new`)
/// — callers should log a warning and continue treating the network as unmetered/unknown.
pub struct NetworkManagerMonitor {
    connection: gio::DBusConnection,
}

impl NetworkManagerMonitor {
    pub fn new() -> Result<Self, NetworkWatchError> {
        let connection = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).map_err(NetworkWatchError::NoSystemBus)?;
        Ok(Self { connection })
    }
}

impl NetworkMonitor for NetworkManagerMonitor {
    fn is_metered(&self) -> Option<bool> {
        use glib::prelude::ToVariant;

        let reply = self
            .connection
            .call_sync(
                Some(NM_BUS_NAME),
                NM_OBJECT_PATH,
                NM_PROPERTIES_IFACE,
                "Get",
                Some(&(NM_BUS_NAME, "Metered").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
            )
            .ok()?;

        // `Properties.Get` replies `(v)` — the property's value boxed in a variant, which needs
        // unwrapping once more to get at the actual `u` (NMMetered) inside.
        let boxed = reply.child_value(0);
        let unboxed = boxed.as_variant()?;
        let code = unboxed.get::<u32>()?;
        metered_code_to_bool(code)
    }
}

fn metered_code_to_bool(code: u32) -> Option<bool> {
    match code {
        NM_METERED_YES | NM_METERED_GUESS_YES => Some(true),
        NM_METERED_NO | NM_METERED_GUESS_NO => Some(false),
        _ => None, // Unknown (0), or any value not in NetworkManager's documented enum.
    }
}

/// The production fallback when `NetworkManagerMonitor::new()` fails (no D-Bus system bus, or
/// none reachable in a sandbox/container) — same "unavailable, not fatal" tolerance as `player`'s
/// own `NullBackend` for a failed `GstBackend::new()`. Always reports "undeterminable" rather than
/// guessing, so a caller gating downloads on `wifi_only` never blocks a download over a check that
/// was never actually possible.
pub struct UnknownNetworkMonitor;

impl NetworkMonitor for UnknownNetworkMonitor {
    fn is_metered(&self) -> Option<bool> {
        None
    }
}

#[cfg(test)]
pub struct FakeNetworkMonitor {
    pub metered: Option<bool>,
}

#[cfg(test)]
impl NetworkMonitor for FakeNetworkMonitor {
    fn is_metered(&self) -> Option<bool> {
        self.metered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metered_code_to_bool_recognizes_yes_and_guess_yes() {
        assert_eq!(metered_code_to_bool(NM_METERED_YES), Some(true));
        assert_eq!(metered_code_to_bool(NM_METERED_GUESS_YES), Some(true));
    }

    #[test]
    fn metered_code_to_bool_recognizes_no_and_guess_no() {
        assert_eq!(metered_code_to_bool(NM_METERED_NO), Some(false));
        assert_eq!(metered_code_to_bool(NM_METERED_GUESS_NO), Some(false));
    }

    #[test]
    fn metered_code_to_bool_treats_unknown_as_undeterminable() {
        assert_eq!(metered_code_to_bool(0), None);
    }

    #[test]
    fn metered_code_to_bool_treats_an_unrecognized_value_as_undeterminable() {
        assert_eq!(metered_code_to_bool(99), None, "a future NetworkManager enum value this code doesn't know about must not be guessed at");
    }

    #[test]
    fn fake_monitor_reports_whatever_it_was_configured_with() {
        assert_eq!(FakeNetworkMonitor { metered: Some(true) }.is_metered(), Some(true));
        assert_eq!(FakeNetworkMonitor { metered: None }.is_metered(), None);
    }

    /// Registers against a *real* system bus — only checkable when one is reachable. Confirmed
    /// live that this sandbox has no system bus socket at all (`call_watch.rs`'s own test
    /// documents the identical finding), so `NetworkManagerMonitor::new()` correctly returns `Err`
    /// rather than panicking. Run explicitly via `cargo test -p abs-player -- --ignored
    /// network_watch::tests::new_does_not` once a system bus is confirmed present.
    #[test]
    #[ignore]
    fn new_does_not_error_even_with_no_networkmanager_present() {
        let monitor = NetworkManagerMonitor::new().expect("connecting to the system bus should succeed");
        let _ = monitor.is_metered();
    }
}
