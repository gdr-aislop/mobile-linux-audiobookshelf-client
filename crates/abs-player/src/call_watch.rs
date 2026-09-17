//! Phone-call interruption — `docs/design/ui-spec.md`'s "Hardware controls & interruptions":
//! watch ModemManager's `org.freedesktop.ModemManager1` voice-call interface and hard-pause
//! playback once a call becomes active. Lives in `abs-player` for the same reason `mpris` does —
//! this is real OS D-Bus integration, not a GTK widget-toolkit concern, and the spec assigns call
//! watching to this crate explicitly.
//!
//! Deliberately **no** "call ended" callback anywhere in this module. Adding one would invite
//! auto-resuming playback when the call ends, which `docs/design/ui-spec.md` explicitly rules
//! out ("no auto-resume"). The only thing a caller can ever be told is "a call just became
//! active" — resuming afterwards is a decision only the person holding the phone gets to make, by
//! pressing play again.

/// ModemManager's `MMCallState` enum value for an active (connected, audio flowing) call — see
/// ModemManager's own D-Bus API reference for `org.freedesktop.ModemManager1.Call`'s `State`
/// property. Ringing (`RINGING_IN`/`RINGING_OUT`, 2/3) and dialing (1) deliberately do *not*
/// trigger a pause here — only a call that's actually connected should interrupt audio, matching
/// how a real phone call would only duck/pause media once you actually pick up.
const MM_CALL_STATE_ACTIVE: i32 = 4;

/// Watches for an incoming/outgoing call becoming active. Implemented by
/// [`ModemManagerCallWatcher`] for real hardware and a `FakeCallWatcher` (test-only, in this
/// module's own tests) for the dependency-injection pattern this crate already uses for
/// [`crate::AudioBackend`].
pub trait CallWatcher {
    /// Registers the callback and starts watching. Calling this more than once replaces any
    /// previously registered callback (mirrors `AudioBackend`'s single-owner shape — there's only
    /// ever one thing that should react to a call going active).
    fn start(&mut self, on_call_active: Box<dyn Fn()>);
}

#[derive(Debug, thiserror::Error)]
pub enum CallWatchError {
    #[error("couldn't reach the D-Bus system bus: {0}")]
    NoSystemBus(glib::Error),
}

/// Watches ModemManager's system-bus interface. Connecting to the system bus (not session —
/// ModemManager is a system service, unlike MPRIS's session-bus `mpris` module) can fail in a
/// sandbox or container with no D-Bus system bus at all; `new()` returns `Err` rather than
/// panicking, matching this crate's existing tolerance (`GstBackend::new()`'s fallback,
/// `mpris::register`'s own graceful `Err`) — callers should log a warning and continue without
/// call-interruption support.
///
/// Implemented against ModemManager's *documented* D-Bus interface — there's no ModemManager, no
/// system bus with a real modem, and no phone hardware in the environment this was implemented
/// in, so this must be confirmed on real hardware (a Librem 5 or similar under phosh, with an
/// actual incoming call) before being considered fully verified, not just "written against the
/// spec".
pub struct ModemManagerCallWatcher {
    connection: gio::DBusConnection,
    subscription: Option<gio::SignalSubscriptionId>,
}

impl ModemManagerCallWatcher {
    pub fn new() -> Result<Self, CallWatchError> {
        let connection = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).map_err(CallWatchError::NoSystemBus)?;
        Ok(Self { connection, subscription: None })
    }
}

impl CallWatcher for ModemManagerCallWatcher {
    fn start(&mut self, on_call_active: Box<dyn Fn()>) {
        if let Some(previous) = self.subscription.take() {
            self.connection.signal_unsubscribe(previous);
        }

        // Every `org.freedesktop.ModemManager1.Call` object announces its state via the standard
        // `org.freedesktop.DBus.Properties.PropertiesChanged` signal, whose first argument is the
        // interface name — `arg0` below is a server-side match on exactly that, so this doesn't
        // need to enumerate modems/calls via `GetManagedObjects` first, and keeps working across
        // however many calls or modems come and go.
        let id = self.connection.signal_subscribe(
            Some("org.freedesktop.ModemManager1"),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            None,
            Some("org.freedesktop.ModemManager1.Call"),
            gio::DBusSignalFlags::NONE,
            move |_conn, _sender, _path, _iface, _signal, params| {
                if call_became_active(params) {
                    on_call_active();
                }
            },
        );
        self.subscription = Some(id);
    }
}

impl Drop for ModemManagerCallWatcher {
    fn drop(&mut self) {
        if let Some(id) = self.subscription.take() {
            self.connection.signal_unsubscribe(id);
        }
    }
}

/// `params` is `PropertiesChanged`'s standard `(interface_name: s, changed_properties: a{sv},
/// invalidated_properties: as)` — this only cares whether `changed_properties` contains `State`
/// set to [`MM_CALL_STATE_ACTIVE`]. ModemManager documents `State` as a signed `i` (not `u`), but
/// this checks both to be tolerant of any implementation that differs.
fn call_became_active(params: &glib::Variant) -> bool {
    let Some(changed) = params.child_value(1).get::<glib::VariantDict>() else { return false };
    let Some(state) = changed.lookup_value("State", None) else { return false };
    state.get::<i32>().or_else(|| state.get::<u32>().map(|v| v as i32)) == Some(MM_CALL_STATE_ACTIVE)
}

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;

#[cfg(test)]
pub struct FakeCallWatcher {
    callback: RefCell<Option<Box<dyn Fn()>>>,
}

#[cfg(test)]
impl FakeCallWatcher {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { callback: RefCell::new(None) })
    }

    pub fn simulate_call_active(&self) {
        if let Some(callback) = self.callback.borrow().as_ref() {
            callback();
        }
    }
}

#[cfg(test)]
impl CallWatcher for Rc<FakeCallWatcher> {
    fn start(&mut self, on_call_active: Box<dyn Fn()>) {
        *self.callback.borrow_mut() = Some(on_call_active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gio::prelude::*;
    use std::cell::Cell;

    #[test]
    fn fake_watcher_invokes_the_callback_on_simulated_call() {
        let watcher = FakeCallWatcher::new();
        let mut watcher_ref = watcher.clone();
        let called = Rc::new(Cell::new(false));
        watcher_ref.start({
            let called = called.clone();
            Box::new(move || called.set(true))
        });

        assert!(!called.get());
        watcher.simulate_call_active();
        assert!(called.get());
    }

    #[test]
    fn starting_again_replaces_the_previous_callback() {
        let watcher = FakeCallWatcher::new();
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

        watcher.simulate_call_active();
        assert!(!first_called.get(), "the first callback should have been replaced");
        assert!(second_called.get());
    }

    #[test]
    fn call_became_active_recognizes_the_active_state() {
        let changed = glib::VariantDict::new(None);
        changed.insert("State", 4_i32);
        let params = glib::Variant::tuple_from_iter([
            "org.freedesktop.ModemManager1.Call".to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        assert!(call_became_active(&params));
    }

    #[test]
    fn call_became_active_ignores_ringing_and_dialing() {
        for ringing_or_dialing_state in [1_i32, 2, 3] {
            let changed = glib::VariantDict::new(None);
            changed.insert("State", ringing_or_dialing_state);
            let params = glib::Variant::tuple_from_iter([
                "org.freedesktop.ModemManager1.Call".to_variant(),
                changed.end(),
                Vec::<String>::new().to_variant(),
            ]);
            assert!(!call_became_active(&params), "state {ringing_or_dialing_state} should not trigger a pause");
        }
    }

    #[test]
    fn call_became_active_ignores_unrelated_property_changes() {
        let changed = glib::VariantDict::new(None);
        changed.insert("Direction", 1_i32);
        let params = glib::Variant::tuple_from_iter([
            "org.freedesktop.ModemManager1.Call".to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        assert!(!call_became_active(&params));
    }

    /// Registers against a *real* system bus — only checkable when one is reachable. Confirmed
    /// live that this sandbox has no system bus socket at all (`Could not connect: No such file
    /// or directory`), so `ModemManagerCallWatcher::new()` correctly returns `Err` rather than
    /// panicking — exactly the graceful-degradation path this type exists for, exercised for
    /// real, just not the "system bus present but no ModemManager" case a target device with a
    /// system bus but no modem would hit. Run explicitly via `cargo test -p abs-player --
    /// --ignored call_watch::tests::new_does_not` once a system bus is confirmed present.
    #[test]
    #[ignore]
    fn new_does_not_error_even_with_no_modemmanager_present() {
        let mut watcher = ModemManagerCallWatcher::new().expect("connecting to the system bus should succeed");
        watcher.start(Box::new(|| {}));
    }
}
