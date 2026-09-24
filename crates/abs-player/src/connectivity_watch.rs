//! Reconnect detection — nothing else in this workspace proactively notices "the network is back
//! up": every other network-touching path is purely reactive (try the call, fall back to local
//! data if it fails), so a device that goes offline while paused/idle has no pending local
//! progress pushed to the server until the user happens to trigger another write. Lives in
//! `abs-player` for the same reason `call_watch`/`route_watch`/`network_watch` do — real OS/D-Bus
//! integration, not a GTK widget-toolkit concern.
//!
//! Watches NetworkManager's own `Connectivity` property (`org.freedesktop.NetworkManager`) rather
//! than a per-device "State" — `Connectivity` is exactly the semantic question "can we actually
//! reach the internet" needs, already accounting for a portal/captive-login page (`PORTAL`) or a
//! walled-garden connection (`LIMITED`) that a device/interface coming up wouldn't distinguish.
//! Only a transition *to* `FULL` fires the callback; going the other way (losing connectivity) is
//! never interesting here — nothing needs to react to going offline, only to coming back.

const NM_BUS_NAME: &str = "org.freedesktop.NetworkManager";

/// NetworkManager's `NMConnectivityState` enum value for "full connectivity, unrestricted access
/// to the internet" — see `org.freedesktop.NetworkManager`'s own D-Bus API reference for the
/// `Connectivity` property. `UNKNOWN` (0), `NONE` (1), `PORTAL` (2) and `LIMITED` (3) deliberately
/// don't get their own match arms below — only a transition all the way to `FULL` is "reconnected"
/// for this module's purposes.
const NM_CONNECTIVITY_FULL: u32 = 4;

/// Watches for connectivity being restored. Implemented by [`NetworkManagerConnectivityWatcher`]
/// for real systems and a `FakeConnectivityWatcher` (test-only, in this module's own tests) for
/// the dependency-injection pattern this crate already uses for [`crate::call_watch::CallWatcher`].
pub trait ConnectivityWatcher {
    /// Registers the callback and starts watching. Calling this more than once replaces any
    /// previously registered callback (mirrors [`crate::call_watch::CallWatcher::start`]).
    fn start(&mut self, on_connectivity_restored: Box<dyn Fn()>);
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectivityWatchError {
    #[error("couldn't reach the D-Bus system bus: {0}")]
    NoSystemBus(glib::Error),
}

/// Watches NetworkManager's system-bus interface. Connecting to the system bus can fail in a
/// sandbox or container with none at all; `new()` returns `Err` rather than panicking, matching
/// this crate's existing tolerance (`GstBackend::new()`, `ModemManagerCallWatcher::new`,
/// `NetworkManagerMonitor::new`) — callers should log a warning and continue without
/// reconnect-triggered sync.
pub struct NetworkManagerConnectivityWatcher {
    connection: gio::DBusConnection,
    subscription: Option<gio::SignalSubscriptionId>,
}

impl NetworkManagerConnectivityWatcher {
    pub fn new() -> Result<Self, ConnectivityWatchError> {
        let connection = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).map_err(ConnectivityWatchError::NoSystemBus)?;
        Ok(Self { connection, subscription: None })
    }
}

impl ConnectivityWatcher for NetworkManagerConnectivityWatcher {
    fn start(&mut self, on_connectivity_restored: Box<dyn Fn()>) {
        if let Some(previous) = self.subscription.take() {
            self.connection.signal_unsubscribe(previous);
        }

        // `org.freedesktop.NetworkManager` itself (not a per-device object) announces its own
        // `Connectivity` property via the standard `org.freedesktop.DBus.Properties.PropertiesChanged`
        // signal — `arg0` below is a server-side match on the interface name, same shape
        // `call_watch.rs` uses for ModemManager's per-call objects.
        let id = self.connection.signal_subscribe(
            Some(NM_BUS_NAME),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            None,
            Some(NM_BUS_NAME),
            gio::DBusSignalFlags::NONE,
            move |_conn, _sender, _path, _iface, _signal, params| {
                if connectivity_became_full(params) {
                    on_connectivity_restored();
                }
            },
        );
        self.subscription = Some(id);
    }
}

impl Drop for NetworkManagerConnectivityWatcher {
    fn drop(&mut self) {
        if let Some(id) = self.subscription.take() {
            self.connection.signal_unsubscribe(id);
        }
    }
}

/// `params` is `PropertiesChanged`'s standard `(interface_name: s, changed_properties: a{sv},
/// invalidated_properties: as)` — this only cares whether `changed_properties` contains
/// `Connectivity` set to [`NM_CONNECTIVITY_FULL`]. NetworkManager documents this as an unsigned
/// `u`, but this checks both to be tolerant of any implementation that differs (same defensive
/// shape as `call_watch.rs`'s `call_became_active`).
fn connectivity_became_full(params: &glib::Variant) -> bool {
    use glib::prelude::ToVariant;
    let Some(changed) = params.child_value(1).get::<glib::VariantDict>() else { return false };
    let Some(value) = changed.lookup_value("Connectivity", None) else { return false };
    value.get::<u32>().or_else(|| value.get::<i32>().map(|v| v as u32)) == Some(NM_CONNECTIVITY_FULL)
        || value == NM_CONNECTIVITY_FULL.to_variant()
}

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;

#[cfg(test)]
pub struct FakeConnectivityWatcher {
    callback: RefCell<Option<Box<dyn Fn()>>>,
}

#[cfg(test)]
impl FakeConnectivityWatcher {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { callback: RefCell::new(None) })
    }

    pub fn simulate_connectivity_restored(&self) {
        if let Some(callback) = self.callback.borrow().as_ref() {
            callback();
        }
    }
}

#[cfg(test)]
impl ConnectivityWatcher for Rc<FakeConnectivityWatcher> {
    fn start(&mut self, on_connectivity_restored: Box<dyn Fn()>) {
        *self.callback.borrow_mut() = Some(on_connectivity_restored);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gio::prelude::*;
    use std::cell::Cell;

    #[test]
    fn fake_watcher_invokes_the_callback_on_simulated_connectivity_restored() {
        let watcher = FakeConnectivityWatcher::new();
        let mut watcher_ref = watcher.clone();
        let called = Rc::new(Cell::new(false));
        watcher_ref.start({
            let called = called.clone();
            Box::new(move || called.set(true))
        });

        assert!(!called.get());
        watcher.simulate_connectivity_restored();
        assert!(called.get());
    }

    #[test]
    fn starting_again_replaces_the_previous_callback() {
        let watcher = FakeConnectivityWatcher::new();
        let mut watcher_ref = watcher.clone();
        let first_called = Rc::new(Cell::new(false));
        let second_called = Rc::new(Cell::new(false));

        watcher_ref.start({
            let first_called = first_called.clone();
            Box::new(move || first_called.set(true))
        });
        watcher_ref.start({
            let second_called = second_called.clone();
            Box::new(move || second_called.set(true))
        });

        watcher.simulate_connectivity_restored();
        assert!(!first_called.get(), "the first callback should have been replaced");
        assert!(second_called.get());
    }

    #[test]
    fn connectivity_became_full_recognizes_the_full_state() {
        let changed = glib::VariantDict::new(None);
        changed.insert("Connectivity", NM_CONNECTIVITY_FULL);
        let params = glib::Variant::tuple_from_iter([
            NM_BUS_NAME.to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        assert!(connectivity_became_full(&params));
    }

    #[test]
    fn connectivity_became_full_ignores_portal_and_limited() {
        for not_full in [0_u32, 1, 2, 3] {
            let changed = glib::VariantDict::new(None);
            changed.insert("Connectivity", not_full);
            let params = glib::Variant::tuple_from_iter([
                NM_BUS_NAME.to_variant(),
                changed.end(),
                Vec::<String>::new().to_variant(),
            ]);
            assert!(!connectivity_became_full(&params), "state {not_full} should not fire a reconnect sync");
        }
    }

    #[test]
    fn connectivity_became_full_ignores_unrelated_property_changes() {
        let changed = glib::VariantDict::new(None);
        changed.insert("State", 70_u32);
        let params = glib::Variant::tuple_from_iter([
            NM_BUS_NAME.to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        assert!(!connectivity_became_full(&params));
    }

    /// Registers against a *real* system bus — only checkable when one is reachable. Confirmed
    /// live that this sandbox has no system bus socket at all (`call_watch.rs`'s own test
    /// documents the identical finding), so `NetworkManagerConnectivityWatcher::new()` correctly
    /// returns `Err` rather than panicking. Run explicitly via `cargo test -p abs-player --
    /// --ignored connectivity_watch::tests::new_does_not` once a system bus is confirmed present.
    #[test]
    #[ignore]
    fn new_does_not_error_even_with_no_networkmanager_present() {
        let mut watcher = NetworkManagerConnectivityWatcher::new().expect("connecting to the system bus should succeed");
        watcher.start(Box::new(|| {}));
    }
}
