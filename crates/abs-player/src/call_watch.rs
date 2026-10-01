//! Phone-call interruption — `docs/design/ui-spec.md`'s "Hardware controls & interruptions":
//! watch ModemManager's `org.freedesktop.ModemManager1` voice-call interface and tell the player
//! when a call starts ringing (or dialing), when it's picked up, and when the last call is gone.
//! Lives in `abs-player` for the same reason `mpris` does — this is real OS D-Bus integration, not
//! a GTK widget-toolkit concern, and the spec assigns call watching to this crate explicitly.
//!
//! What the player does with these is its own business (`PlayerController::handle_call_event`):
//! pause as soon as a call rings, resume only when an incoming call ended without ever being
//! picked up (rejected, missed, or the caller gave up), and stay paused after a call that was
//! actually answered. This module only reports facts — it never decides to resume anything.
//!
//! Why `CallAdded` matters: ModemManager creates an incoming call object already in the
//! `RINGING_IN` state and only then exports it, so no `StateChanged`/`PropertiesChanged` signal
//! is ever emitted for the ringing itself — the first state *change* a watcher can see is the
//! pickup (`ACTIVE`) or the hang-up (`TERMINATED`). Watching only state changes is exactly what
//! made the first version of this module pause on pickup instead of on ringing (the Librem 5
//! field report). So a new call is picked up from the voice interface's `CallAdded` signal and
//! its current state is read with `Properties.Get`; later transitions come from the call's own
//! signals.
//!
//! Every observation is logged at info level — "phone call state" for each raw ModemManager
//! report, "phone call event" for what was derived from it — the same shape as `route_watch`'s
//! "headphone route changed", so a device that doesn't react to a call can be diagnosed from the
//! log alone.

use glib::prelude::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const MM_BUS_NAME: &str = "org.freedesktop.ModemManager1";
const MM_VOICE_INTERFACE: &str = "org.freedesktop.ModemManager1.Modem.Voice";
const MM_CALL_INTERFACE: &str = "org.freedesktop.ModemManager1.Call";

/// ModemManager's `MMCallState` values — see ModemManager's D-Bus API reference for
/// `org.freedesktop.ModemManager1.Call`'s `State` property.
const MM_CALL_STATE_UNKNOWN: i32 = 0;
const MM_CALL_STATE_DIALING: i32 = 1;
const MM_CALL_STATE_RINGING_OUT: i32 = 2;
const MM_CALL_STATE_RINGING_IN: i32 = 3;
const MM_CALL_STATE_ACTIVE: i32 = 4;
const MM_CALL_STATE_HELD: i32 = 5;
const MM_CALL_STATE_WAITING: i32 = 6;
const MM_CALL_STATE_TERMINATED: i32 = 7;

/// A human-readable name for an `MMCallState`, for the log.
fn state_name(state: i32) -> &'static str {
    match state {
        MM_CALL_STATE_UNKNOWN => "unknown",
        MM_CALL_STATE_DIALING => "dialing",
        MM_CALL_STATE_RINGING_OUT => "ringing-out",
        MM_CALL_STATE_RINGING_IN => "ringing-in",
        MM_CALL_STATE_ACTIVE => "active",
        MM_CALL_STATE_HELD => "held",
        MM_CALL_STATE_WAITING => "waiting",
        MM_CALL_STATE_TERMINATED => "terminated",
        _ => "unrecognized",
    }
}

/// What the player is told about phone calls. Calls are grouped into an *episode*: from the
/// first call appearing to the last one going away — so a second call waiting during the first
/// doesn't produce a second `Started`, and `Ended` only fires once nothing is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEvent {
    /// A call episode began: an incoming call started ringing (`incoming: true`), or an outgoing
    /// one started dialing.
    Started { incoming: bool },
    /// A call was picked up — audio is now connected.
    Answered,
    /// The last call is gone. `answered`: any call in the episode was connected at some point.
    /// `outgoing`: any call in the episode was one this phone placed. Both `false` means only
    /// unanswered incoming calls happened — rejected, missed, or the caller hung up first.
    Ended { answered: bool, outgoing: bool },
}

/// Watches for phone calls. Implemented by [`ModemManagerCallWatcher`] for real hardware and a
/// `FakeCallWatcher` (test-only, in this module's own tests) for the dependency-injection pattern
/// this crate already uses for [`crate::AudioBackend`].
pub trait CallWatcher {
    /// Registers the callback and starts watching. Calling this more than once replaces any
    /// previously registered callback (mirrors `AudioBackend`'s single-owner shape — there's only
    /// ever one thing that should react to a call).
    fn start(&mut self, on_event: Box<dyn Fn(CallEvent)>);
}

#[derive(Debug, thiserror::Error)]
pub enum CallWatchError {
    #[error("couldn't reach the D-Bus system bus: {0}")]
    NoSystemBus(glib::Error),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct CallRecord {
    answered: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Episode {
    answered: bool,
    outgoing: bool,
}

/// Turns ModemManager's per-call state reports into [`CallEvent`]s — a pure, synchronous state
/// machine so the rules are unit-testable without a modem.
///
/// Observations may repeat and may arrive out of order (the `Get` reply for a new call races
/// that call's own signals, and both `StateChanged` and `PropertiesChanged` report the same
/// transition), so every input is idempotent: "answered" is sticky, and a call that already ended
/// is remembered so a late, stale read can't bring it back.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CallTracker {
    calls: BTreeMap<String, CallRecord>,
    finished: BTreeSet<String>,
    episode: Option<Episode>,
}

impl CallTracker {
    /// Feeds "call `path` is in `state`".
    pub(crate) fn observe(&mut self, path: &str, state: i32) -> Vec<CallEvent> {
        if state == MM_CALL_STATE_TERMINATED {
            return self.removed(path);
        }
        if self.finished.contains(path) {
            return Vec::new();
        }
        let incoming = matches!(state, MM_CALL_STATE_RINGING_IN | MM_CALL_STATE_WAITING);
        let outgoing = matches!(state, MM_CALL_STATE_DIALING | MM_CALL_STATE_RINGING_OUT);
        let connected = matches!(state, MM_CALL_STATE_ACTIVE | MM_CALL_STATE_HELD);
        let in_progress = incoming || outgoing || connected;
        // An outgoing call is created in `UNKNOWN` and moves to `DIALING` once started — nothing
        // is happening yet, so it doesn't open an episode.
        if !in_progress && !self.calls.contains_key(path) {
            return Vec::new();
        }

        let mut events = Vec::new();
        if self.episode.is_none() {
            self.episode = Some(Episode::default());
            events.push(CallEvent::Started { incoming: !outgoing });
        }
        let episode = self.episode.as_mut().expect("just ensured");
        episode.outgoing |= outgoing;
        let record = self.calls.entry(path.to_string()).or_default();
        if connected && !record.answered {
            record.answered = true;
            episode.answered = true;
            events.push(CallEvent::Answered);
        }
        events
    }

    /// Feeds "call `path` is gone" — `TERMINATED`, or the voice interface's `CallDeleted`.
    pub(crate) fn removed(&mut self, path: &str) -> Vec<CallEvent> {
        self.finished.insert(path.to_string());
        if self.calls.remove(path).is_none() || !self.calls.is_empty() {
            return Vec::new();
        }
        match self.episode.take() {
            Some(episode) => vec![CallEvent::Ended { answered: episode.answered, outgoing: episode.outgoing }],
            None => Vec::new(),
        }
    }
}

/// Watches ModemManager's system-bus interface. Connecting to the system bus (not session —
/// ModemManager is a system service, unlike MPRIS's session-bus `mpris` module) can fail in a
/// sandbox or container with no D-Bus system bus at all; `new()` returns `Err` rather than
/// panicking, matching this crate's existing tolerance (`GstBackend::new()`'s fallback,
/// `mpris::register`'s own graceful `Err`) — callers should log a warning and continue without
/// call-interruption support.
///
/// The signal callbacks run on the GLib main context `start()` is called from (the app's main
/// loop), so the tracker and the caller's callback can be plain `Rc`/non-`Send` state.
pub struct ModemManagerCallWatcher {
    connection: gio::DBusConnection,
    subscriptions: Vec<gio::SignalSubscriptionId>,
}

impl ModemManagerCallWatcher {
    pub fn new() -> Result<Self, CallWatchError> {
        let connection = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).map_err(CallWatchError::NoSystemBus)?;
        Ok(Self { connection, subscriptions: Vec::new() })
    }

    /// Watches over an already-open connection — for the tests, which run a fake ModemManager
    /// on a private bus.
    #[cfg(test)]
    fn with_connection(connection: gio::DBusConnection) -> Self {
        Self { connection, subscriptions: Vec::new() }
    }

    fn unsubscribe_all(&mut self) {
        for id in self.subscriptions.drain(..) {
            self.connection.signal_unsubscribe(id);
        }
    }
}

/// Shared between every signal subscription: the tracker plus the caller's callback.
struct WatchState {
    tracker: RefCell<CallTracker>,
    on_event: Box<dyn Fn(CallEvent)>,
}

impl WatchState {
    fn deliver(&self, events: Vec<CallEvent>) {
        for event in events {
            tracing::info!(?event, "phone call event");
            (self.on_event)(event);
        }
    }

    fn observe(&self, path: &str, state: i32, source: &str) {
        tracing::info!(call = path, state = state_name(state), state_code = state, source, "phone call state");
        let events = self.tracker.borrow_mut().observe(path, state);
        self.deliver(events);
    }

    fn removed(&self, path: &str) {
        tracing::info!(call = path, "phone call removed");
        let events = self.tracker.borrow_mut().removed(path);
        self.deliver(events);
    }
}

impl CallWatcher for ModemManagerCallWatcher {
    fn start(&mut self, on_event: Box<dyn Fn(CallEvent)>) {
        self.unsubscribe_all();
        let state = Rc::new(WatchState { tracker: RefCell::new(CallTracker::default()), on_event });

        // A new call: its ringing state was set before it was exported (see the module docs), so
        // read it rather than waiting for a change that never comes.
        let added = self.connection.signal_subscribe(
            Some(MM_BUS_NAME),
            Some(MM_VOICE_INTERFACE),
            Some("CallAdded"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            {
                let state = state.clone();
                move |conn, _sender, _path, _iface, _signal, params| {
                    let Some((call_path,)) = params.get::<(glib::variant::ObjectPath,)>() else { return };
                    let call_path = call_path.as_str().to_string();
                    tracing::info!(call = %call_path, "phone call added");
                    let state = state.clone();
                    let object_path = call_path.clone();
                    conn.call(
                        Some(MM_BUS_NAME),
                        &object_path,
                        "org.freedesktop.DBus.Properties",
                        "Get",
                        Some(&(MM_CALL_INTERFACE, "State").to_variant()),
                        Some(glib::VariantTy::new("(v)").expect("valid type string")),
                        gio::DBusCallFlags::NONE,
                        -1,
                        gio::Cancellable::NONE,
                        move |result| match result.map(|reply| call_state_from_get_reply(&reply)) {
                            Ok(Some(call_state)) => state.observe(&call_path, call_state, "added"),
                            Ok(None) => tracing::warn!(call = %call_path, "a new phone call's State couldn't be read"),
                            Err(err) => tracing::warn!(call = %call_path, %err, "couldn't read a new phone call's State"),
                        },
                    );
                }
            },
        );

        let deleted = self.connection.signal_subscribe(
            Some(MM_BUS_NAME),
            Some(MM_VOICE_INTERFACE),
            Some("CallDeleted"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            {
                let state = state.clone();
                move |_conn, _sender, _path, _iface, _signal, params| {
                    if let Some((call_path,)) = params.get::<(glib::variant::ObjectPath,)>() {
                        state.removed(call_path.as_str());
                    }
                }
            },
        );

        // `StateChanged(i old, i new, u reason)` — the call's own explicit transition signal.
        let state_changed = self.connection.signal_subscribe(
            Some(MM_BUS_NAME),
            Some(MM_CALL_INTERFACE),
            Some("StateChanged"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            {
                let state = state.clone();
                move |_conn, _sender, path, _iface, _signal, params| {
                    if let Some((_old, new, _reason)) = params.get::<(i32, i32, u32)>() {
                        state.observe(path, new, "state-changed");
                    }
                }
            },
        );

        // The same transition as a standard property change — kept alongside `StateChanged` in
        // case an implementation only emits one of them; the tracker is idempotent, so seeing
        // both is harmless. `arg0` is a server-side match on the interface name.
        let properties_changed = self.connection.signal_subscribe(
            Some(MM_BUS_NAME),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            None,
            Some(MM_CALL_INTERFACE),
            gio::DBusSignalFlags::NONE,
            {
                let state = state.clone();
                move |_conn, _sender, path, _iface, _signal, params| {
                    if let Some(call_state) = call_state_from_properties_changed(params) {
                        state.observe(path, call_state, "properties-changed");
                    }
                }
            },
        );

        self.subscriptions = vec![added, deleted, state_changed, properties_changed];
        tracing::info!("watching ModemManager for phone calls");
    }
}

impl Drop for ModemManagerCallWatcher {
    fn drop(&mut self) {
        self.unsubscribe_all();
    }
}

/// Reads an `MMCallState` out of a variant. ModemManager documents `State` as a signed `i` (not
/// `u`), but this accepts both to be tolerant of any implementation that differs.
fn call_state_from_variant(value: &glib::Variant) -> Option<i32> {
    value.get::<i32>().or_else(|| value.get::<u32>().map(|v| v as i32))
}

/// `Properties.Get`'s reply is `(v)`.
fn call_state_from_get_reply(reply: &glib::Variant) -> Option<i32> {
    let boxed = reply.try_child_value(0)?.as_variant()?;
    call_state_from_variant(&boxed)
}

/// `params` is `PropertiesChanged`'s standard `(interface_name: s, changed_properties: a{sv},
/// invalidated_properties: as)` — `None` unless `changed_properties` contains `State`.
fn call_state_from_properties_changed(params: &glib::Variant) -> Option<i32> {
    let changed = params.try_child_value(1)?.get::<glib::VariantDict>()?;
    let state = changed.lookup_value("State", None)?;
    call_state_from_variant(&state)
}

#[cfg(test)]
type EventCallback = Box<dyn Fn(CallEvent)>;

#[cfg(test)]
pub struct FakeCallWatcher {
    callback: RefCell<Option<EventCallback>>,
}

#[cfg(test)]
impl FakeCallWatcher {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { callback: RefCell::new(None) })
    }

    pub fn simulate(&self, event: CallEvent) {
        if let Some(callback) = self.callback.borrow().as_ref() {
            callback(event);
        }
    }
}

#[cfg(test)]
impl CallWatcher for Rc<FakeCallWatcher> {
    fn start(&mut self, on_event: Box<dyn Fn(CallEvent)>) {
        *self.callback.borrow_mut() = Some(on_event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const CALL_1: &str = "/org/freedesktop/ModemManager1/Call/1";
    const CALL_2: &str = "/org/freedesktop/ModemManager1/Call/2";

    #[test]
    fn fake_watcher_invokes_the_callback_on_simulated_call() {
        let watcher = FakeCallWatcher::new();
        let mut watcher_ref = watcher.clone();
        let seen = Rc::new(RefCell::new(Vec::new()));
        watcher_ref.start({
            let seen = seen.clone();
            Box::new(move |event| seen.borrow_mut().push(event))
        });

        watcher.simulate(CallEvent::Started { incoming: true });
        assert_eq!(*seen.borrow(), vec![CallEvent::Started { incoming: true }]);
    }

    #[test]
    fn starting_again_replaces_the_previous_callback() {
        let watcher = FakeCallWatcher::new();
        let mut watcher_ref = watcher.clone();
        let first_called = Rc::new(Cell::new(false));
        let second_called = Rc::new(Cell::new(false));

        watcher_ref.start({
            let first_called = first_called.clone();
            Box::new(move |_| first_called.set(true))
        });
        watcher_ref.start({
            let second_called = second_called.clone();
            Box::new(move |_| second_called.set(true))
        });

        watcher.simulate(CallEvent::Answered);
        assert!(!first_called.get(), "the first callback should have been replaced");
        assert!(second_called.get());
    }

    /// The field report's case: the call is first seen already ringing (via `CallAdded` +
    /// `Get`), and rejecting it ends the episode as unanswered and incoming.
    #[test]
    fn a_rejected_incoming_call_starts_on_ringing_and_ends_unanswered() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN), vec![CallEvent::Started { incoming: true }]);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_TERMINATED), vec![CallEvent::Ended { answered: false, outgoing: false }]);
    }

    #[test]
    fn an_answered_call_reports_the_pickup_and_ends_answered() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN), vec![CallEvent::Started { incoming: true }]);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_ACTIVE), vec![CallEvent::Answered]);
        // The same transition again (StateChanged and PropertiesChanged both report it).
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_ACTIVE), vec![]);
        assert_eq!(tracker.removed(CALL_1), vec![CallEvent::Ended { answered: true, outgoing: false }]);
    }

    #[test]
    fn an_outgoing_call_starts_on_dialing_not_on_creation() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_UNKNOWN), vec![]);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_DIALING), vec![CallEvent::Started { incoming: false }]);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_OUT), vec![]);
        assert_eq!(tracker.removed(CALL_1), vec![CallEvent::Ended { answered: false, outgoing: true }]);
    }

    #[test]
    fn a_call_first_seen_already_active_starts_and_answers_at_once() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_ACTIVE), vec![CallEvent::Started { incoming: true }, CallEvent::Answered]);
    }

    /// `CallDeleted` usually follows `TERMINATED`; the episode must end exactly once.
    #[test]
    fn terminated_then_deleted_ends_once() {
        let mut tracker = CallTracker::default();
        tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_TERMINATED).len(), 1);
        assert_eq!(tracker.removed(CALL_1), vec![]);
    }

    /// The `Get` reply for a new call can arrive after that call already ended — it must not
    /// resurrect it (which would leave an episode open forever).
    #[test]
    fn a_stale_read_after_the_call_ended_is_ignored() {
        let mut tracker = CallTracker::default();
        tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN);
        tracker.removed(CALL_1);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN), vec![]);
        assert_eq!(tracker, CallTracker { finished: [CALL_1.to_string()].into(), ..CallTracker::default() });
    }

    /// A stale `RINGING_IN` read arriving after the pickup must not un-answer the call.
    #[test]
    fn answered_is_sticky() {
        let mut tracker = CallTracker::default();
        tracker.observe(CALL_1, MM_CALL_STATE_ACTIVE);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN), vec![]);
        assert_eq!(tracker.removed(CALL_1), vec![CallEvent::Ended { answered: true, outgoing: false }]);
    }

    /// A second call waiting during the first is part of the same episode: no second `Started`,
    /// and `Ended` only once both are gone, carrying whether *any* of them was answered.
    #[test]
    fn overlapping_calls_form_one_episode() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_RINGING_IN), vec![CallEvent::Started { incoming: true }]);
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_ACTIVE), vec![CallEvent::Answered]);
        assert_eq!(tracker.observe(CALL_2, MM_CALL_STATE_WAITING), vec![]);
        assert_eq!(tracker.removed(CALL_2), vec![]);
        assert_eq!(tracker.removed(CALL_1), vec![CallEvent::Ended { answered: true, outgoing: false }]);
    }

    #[test]
    fn ending_a_call_never_seen_is_ignored() {
        let mut tracker = CallTracker::default();
        assert_eq!(tracker.observe(CALL_1, MM_CALL_STATE_TERMINATED), vec![]);
        assert_eq!(tracker.removed(CALL_2), vec![]);
    }

    #[test]
    fn reads_state_from_properties_changed() {
        let changed = glib::VariantDict::new(None);
        changed.insert("State", MM_CALL_STATE_RINGING_IN);
        let params = glib::Variant::tuple_from_iter([MM_CALL_INTERFACE.to_variant(), changed.end(), Vec::<String>::new().to_variant()]);
        assert_eq!(call_state_from_properties_changed(&params), Some(MM_CALL_STATE_RINGING_IN));
    }

    #[test]
    fn properties_changed_without_state_is_ignored() {
        let changed = glib::VariantDict::new(None);
        changed.insert("Direction", 1_i32);
        let params = glib::Variant::tuple_from_iter([MM_CALL_INTERFACE.to_variant(), changed.end(), Vec::<String>::new().to_variant()]);
        assert_eq!(call_state_from_properties_changed(&params), None);
    }

    #[test]
    fn reads_state_from_a_get_reply_signed_or_unsigned() {
        let signed = glib::Variant::tuple_from_iter([glib::Variant::from_variant(&MM_CALL_STATE_RINGING_IN.to_variant())]);
        assert_eq!(call_state_from_get_reply(&signed), Some(MM_CALL_STATE_RINGING_IN));
        let unsigned = glib::Variant::tuple_from_iter([glib::Variant::from_variant(&4_u32.to_variant())]);
        assert_eq!(call_state_from_get_reply(&unsigned), Some(MM_CALL_STATE_ACTIVE));
    }

    /// A private `dbus-daemon`, killed on drop — `gio::TestDBus` would do, but its `up()` rewrites
    /// the whole process's `DBUS_SESSION_BUS_ADDRESS`, which other tests running in parallel
    /// threads would then see.
    struct PrivateBus {
        daemon: std::process::Child,
        address: String,
    }

    impl PrivateBus {
        /// `None` when there's no `dbus-daemon` to run.
        fn start() -> Option<Self> {
            use std::io::BufRead;
            let mut daemon = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .ok()?;
            let mut address = String::new();
            std::io::BufReader::new(daemon.stdout.take()?).read_line(&mut address).ok()?;
            Some(Self { daemon, address: address.trim().to_string() })
        }

        fn connect(&self) -> gio::DBusConnection {
            gio::DBusConnection::for_address_sync(
                &self.address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
                gio::Cancellable::NONE,
            )
            .expect("connecting to the private bus")
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    const FAKE_CALL_XML: &str = r#"<node>
      <interface name="org.freedesktop.ModemManager1.Call">
        <property name="State" type="i" access="read"/>
        <signal name="StateChanged"><arg type="i"/><arg type="i"/><arg type="u"/></signal>
      </interface>
    </node>"#;

    /// End-to-end over real D-Bus, against a fake ModemManager that behaves like the real one
    /// on the Librem 5 field report: the incoming call is exported already `RINGING_IN` (no
    /// state-change signal for the ringing at all), and every later transition is reported by
    /// both `StateChanged` and `PropertiesChanged`, followed by `CallDeleted`. The watcher must
    /// report the ringing from `CallAdded` alone, and each transition exactly once.
    #[test]
    fn reports_ringing_from_call_added_and_each_transition_once_over_dbus() {
        let Some(bus) = PrivateBus::start() else {
            eprintln!("skipping: no dbus-daemon to run a private bus");
            return;
        };
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let pump = |until: &dyn Fn() -> bool| {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while !until() && std::time::Instant::now() < deadline {
                        context.iteration(false);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    // Let anything still in flight (a duplicate report) land too.
                    let settle = std::time::Instant::now() + std::time::Duration::from_millis(200);
                    while std::time::Instant::now() < settle {
                        context.iteration(false);
                    }
                };

                let modem_manager = bus.connect();
                modem_manager
                    .call_sync(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "RequestName",
                        Some(&(MM_BUS_NAME, 4_u32).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        -1,
                        gio::Cancellable::NONE,
                    )
                    .expect("owning ModemManager's name");
                let node = gio::DBusNodeInfo::for_xml(FAKE_CALL_XML).expect("valid XML");
                let call_info = node.lookup_interface(MM_CALL_INTERFACE).expect("call interface");
                let states: Rc<RefCell<BTreeMap<String, i32>>> = Rc::default();
                let mut registrations = Vec::new();
                for path in [CALL_1, CALL_2] {
                    registrations.push(
                        modem_manager
                            .register_object(path, &call_info)
                            .property({
                                let states = states.clone();
                                move |_conn, _sender, path, _iface, _property| states.borrow()[path].to_variant()
                            })
                            .build()
                            .expect("registering a fake call"),
                    );
                }
                let modem = "/org/freedesktop/ModemManager1/Modem/0";
                let call_path = |path: &str| (glib::variant::ObjectPath::try_from(path.to_string()).expect("valid path"),).to_variant();
                let set_state = |path: &str, old: i32, new: i32, reason: u32| {
                    states.borrow_mut().insert(path.to_string(), new);
                    modem_manager.emit_signal(None, path, MM_CALL_INTERFACE, "StateChanged", Some(&(old, new, reason).to_variant())).unwrap();
                    let changed = glib::VariantDict::new(None);
                    changed.insert("State", new);
                    let params = glib::Variant::tuple_from_iter([MM_CALL_INTERFACE.to_variant(), changed.end(), Vec::<String>::new().to_variant()]);
                    modem_manager.emit_signal(None, path, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&params)).unwrap();
                };

                let seen = Rc::new(RefCell::new(Vec::new()));
                let mut watcher = ModemManagerCallWatcher::with_connection(bus.connect());
                watcher.start({
                    let seen = seen.clone();
                    Box::new(move |event| seen.borrow_mut().push(event))
                });
                // Let the watcher's match rules (and GDBus's lookup of who owns the name) land.
                pump(&|| false);

                // Rings, then is rejected.
                states.borrow_mut().insert(CALL_1.to_string(), MM_CALL_STATE_RINGING_IN);
                modem_manager.emit_signal(None, modem, MM_VOICE_INTERFACE, "CallAdded", Some(&call_path(CALL_1))).unwrap();
                pump(&|| !seen.borrow().is_empty());
                assert_eq!(*seen.borrow(), vec![CallEvent::Started { incoming: true }], "ringing alone must be reported");
                set_state(CALL_1, MM_CALL_STATE_RINGING_IN, MM_CALL_STATE_TERMINATED, 5);
                modem_manager.emit_signal(None, modem, MM_VOICE_INTERFACE, "CallDeleted", Some(&call_path(CALL_1))).unwrap();
                pump(&|| seen.borrow().len() >= 2);
                assert_eq!(seen.borrow()[1..], [CallEvent::Ended { answered: false, outgoing: false }]);

                // Rings, is picked up, then hung up.
                seen.borrow_mut().clear();
                states.borrow_mut().insert(CALL_2.to_string(), MM_CALL_STATE_RINGING_IN);
                modem_manager.emit_signal(None, modem, MM_VOICE_INTERFACE, "CallAdded", Some(&call_path(CALL_2))).unwrap();
                pump(&|| !seen.borrow().is_empty());
                set_state(CALL_2, MM_CALL_STATE_RINGING_IN, MM_CALL_STATE_ACTIVE, 3);
                pump(&|| seen.borrow().len() >= 2);
                set_state(CALL_2, MM_CALL_STATE_ACTIVE, MM_CALL_STATE_TERMINATED, 4);
                modem_manager.emit_signal(None, modem, MM_VOICE_INTERFACE, "CallDeleted", Some(&call_path(CALL_2))).unwrap();
                pump(&|| seen.borrow().len() >= 3);
                assert_eq!(
                    *seen.borrow(),
                    vec![CallEvent::Started { incoming: true }, CallEvent::Answered, CallEvent::Ended { answered: true, outgoing: false }]
                );

                drop(watcher);
                for id in registrations {
                    modem_manager.unregister_object(id).unwrap();
                }
            })
            .expect("owning a fresh main context");
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
        watcher.start(Box::new(|_| {}));
    }
}
