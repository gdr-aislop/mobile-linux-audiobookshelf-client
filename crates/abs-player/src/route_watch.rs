//! Headphone plug/unplug watching — `docs/design/ui-spec.md`'s "Hardware controls & interruptions":
//! pause playback when the audio output the listener is wearing goes away (wired jack unplugged,
//! Bluetooth headphones disconnected), and optionally resume when it comes back. Lives in
//! `abs-player` for the same reason `mpris` and `call_watch` do — this is real OS audio-server
//! integration, not a GTK widget-toolkit concern.
//!
//! MPRIS can't do any of this: it is an outbound-only control interface (it exposes this app's
//! player state and accepts transport commands from the shell), with no concept of inbound
//! audio-routing events. The routing authority is the audio server — PulseAudio, or PipeWire
//! via its `pipewire-pulse` compatibility socket (the standard mobile-Linux setup) — which
//! reports kernel jack detection as a *port availability* flag on the sink's ports. This module
//! watches that over the PulseAudio client protocol (the same libpulse GStreamer's `pulsesink`
//! — the audio path this app actually plays through — already uses), so it sees both wired jack
//! events and Bluetooth sink appearances/disappearances with one integration.
//!
//! Two known limits, both documented in the spec: a Bluetooth disconnect shows up as the whole
//! sink being removed (always detectable), while a wired unplug needs the hardware to report jack
//! detection — devices whose port reports "unknown" availability (some USB DACs) can't be
//! detected and are ignored. And like `call_watch`, there is deliberately no unconditional
//! auto-resume: resuming after a replug only ever happens when the preceding pause was itself
//! caused by an unplug (see `PlayerController::handle_route_event`), so a manual pause or a
//! phone call is never overridden by a reconnection.
//!
//! Delivered events:
//! - [`RouteEvent::Unplugged`] — the headphone port flipped from available to unavailable, or
//!   the sink we were listening on disappeared (Bluetooth).
//! - [`RouteEvent::Replugged`] — a headphone port became available again. Harmless if nothing
//!   had been unplugged: consumers gate on "did we auto-pause".

/// A change in the availability of the headphone output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteEvent {
    /// The headphone output went away (jack unplugged, Bluetooth device disconnected).
    Unplugged,
    /// A headphone output (re)appeared.
    Replugged,
}

/// Watches for headphone plug/unplug. Implemented by [`PulseRouteWatcher`] for real systems
/// and a `FakeRouteWatcher` (test-only, in this module's tests) for the dependency-injection
/// pattern this crate already uses for [`crate::AudioBackend`] and [`crate::call_watch::CallWatcher`].
pub trait RouteWatcher {
    /// Registers the callback and starts delivering events. Calling this more than once replaces
    /// any previously registered callback (mirrors `CallWatcher`'s single-owner shape — there is
    /// only ever one thing that should react to a route change).
    fn start(&mut self, on_event: Box<dyn Fn(RouteEvent)>);
}

#[derive(Debug, thiserror::Error)]
pub enum RouteWatchError {
    #[error("couldn't connect to the audio server (PulseAudio/PipeWire): {0}")]
    NoAudioServer(String),
}

/// Turns raw audio-server observations into [`RouteEvent`]s — kept as a pure, synchronous state
/// machine so the classification rules are unit-testable without any audio server at all.
///
/// State is just `headphones_present: Option<bool>`, where `None` means "not yet observed". The
/// asymmetry in the transitions is deliberate and load-bearing:
/// - `None -> true` **does** emit `Replugged`: it is what a Bluetooth reconnection looks like
///   (the sink vanished, wiping our knowledge; its return re-reports port availability). It is
///   safe even at startup because consumers only act on `Replugged` when a previous unplug was
///   the reason for the current pause.
/// - `None -> false` emits **nothing**: at startup that is just "headphones are (still)
///   unplugged", and pausing over the state of the world at launch would be absurd.
/// - `Unknown` port availability (devices without jack detection) leaves all state untouched.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteClassifier {
    headphones_present: Option<bool>,
}

/// What a sink's headphone port reports — PulseAudio's `pa_available_t`, minus its numeric
/// identity. `Unknown` means the hardware doesn't do jack detection at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortAvailability {
    Unknown,
    Yes,
    No,
}

impl RouteClassifier {
    /// Feeds the availability of a headphone port. `None` means "no headphone port was found on
    /// this sink" (e.g. a speaker-only output) — indistinguishable from `Unknown` from the
    /// outside, and ignored either way.
    pub(crate) fn on_headphone_port(&mut self, availability: Option<PortAvailability>) -> Option<RouteEvent> {
        let available = match availability {
            Some(PortAvailability::Yes) => true,
            Some(PortAvailability::No) => false,
            Some(PortAvailability::Unknown) | None => return None,
        };
        let event = match (self.headphones_present, available) {
            (Some(true), false) => Some(RouteEvent::Unplugged),
            (Some(false), true) | (None, true) => Some(RouteEvent::Replugged),
            (Some(false), false) | (Some(true), true) | (None, false) => None,
        };
        self.headphones_present = Some(available);
        event
    }

    /// Feeds "a sink disappeared" — how a Bluetooth disconnect manifests. An unplug is reported
    /// only if we currently believe headphones were present; our knowledge is then wiped, so the
    /// sink's return re-reports availability starting from scratch.
    pub(crate) fn on_sink_removed(&mut self) -> Option<RouteEvent> {
        let was_present = self.headphones_present == Some(true);
        self.headphones_present = None;
        was_present.then_some(RouteEvent::Unplugged)
    }
}

/// The real watcher: a background thread owning a libpulse context, subscribed to sink and
/// server events. Introspection results and subscribe events are funneled through a channel and
/// classified between `iterate()` calls (a libpulse callback can't re-enter the context that owns
/// it, so nothing inside the callback ever touches `Context` — it only sends). Classified events
/// cross into the GLib main loop over a `futures` channel, whose receiving future runs there and
/// invokes the caller's callback — keeping that callback free to hold non-`Send` GTK state.
///
/// `new()` blocks briefly (bounded by `INIT_TIMEOUT`) waiting for the connection to reach Ready —
/// the same synchronous-connect shape `call_watch`'s `ModemManagerCallWatcher::new()` uses — and
/// returns [`RouteWatchError::NoAudioServer`] when there is no audio server to reach (sandboxes,
/// CI), so callers can warn and continue without headphone support. The thread outlives nothing:
/// it exits as soon as the context fails or is terminated.
pub struct PulseRouteWatcher {
    sender: std::sync::Arc<std::sync::Mutex<Option<futures::channel::mpsc::UnboundedSender<RouteEvent>>>>,
}

/// How long `PulseRouteWatcher::new()` waits for the audio-server connection to reach Ready.
const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

enum Signal {
    StateChanged,
    SinkListChanged,
    SinkRemoved,
    SinkPorts { availability: Option<PortAvailability> },
    SubscribeFailed,
}

/// Names the headphone-ish ports of a sink. Port names are PulseAudio conventions like
/// `analog-output-headphones` / `analog-output-headset` (the match is case-insensitive to be
/// robust across drivers); Bluetooth sinks have no such ports at all and are covered by
/// sink-removal instead.
fn headphone_availability(sink_ports: &[libpulse_binding::context::introspect::SinkPortInfo<'_>]) -> Option<PortAvailability> {
    sink_ports
        .iter()
        .find(|port| {
            let name = port.name.as_deref().unwrap_or("").to_ascii_lowercase();
            name.contains("headphone") || name.contains("headset")
        })
        .map(|port| match port.available {
            libpulse_binding::def::PortAvailable::Yes => PortAvailability::Yes,
            libpulse_binding::def::PortAvailable::No => PortAvailability::No,
            _ => PortAvailability::Unknown,
        })
}

impl PulseRouteWatcher {
    pub fn new() -> Result<Self, RouteWatchError> {
        use libpulse_binding::callbacks::ListResult;
        use libpulse_binding::context::subscribe::{Facility, InterestMaskSet, Operation};
        use libpulse_binding::context::{Context, FlagSet, State};
        use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};

        let sender: std::sync::Arc<std::sync::Mutex<Option<futures::channel::mpsc::UnboundedSender<RouteEvent>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), RouteWatchError>>();

        let sender_for_thread = sender.clone();
        std::thread::Builder::new()
            .name("abs-route-watch".into())
            .spawn(move || {
                let Some(mut mainloop) = Mainloop::new() else {
                    let _ = init_tx.send(Err(RouteWatchError::NoAudioServer("couldn't create the PulseAudio main loop".into())));
                    return;
                };
                let Some(mut context) = Context::new(&mainloop, "abs-app-route-watch") else {
                    let _ = init_tx.send(Err(RouteWatchError::NoAudioServer("couldn't create the PulseAudio context".into())));
                    return;
                };

                // Everything the libpulse callbacks do is "send a Signal and get out" — a
                // callback running inside `iterate()` must never re-enter the `Context` that owns
                // it. Classification and introspection both happen in this thread's own loop,
                // strictly between `iterate()` calls.
                let (signal_tx, signal_rx) = std::sync::mpsc::channel::<Signal>();
                context.set_state_callback(Some(Box::new({
                    let signal_tx = signal_tx.clone();
                    move || {
                        let _ = signal_tx.send(Signal::StateChanged);
                    }
                })));
                if context.connect(None, FlagSet::NOFLAGS, None).is_err() {
                    let _ = init_tx.send(Err(RouteWatchError::NoAudioServer("connecting to the audio server failed".into())));
                    return;
                }
                context.set_subscribe_callback(Some(Box::new({
                    let signal_tx = signal_tx.clone();
                    move |facility, operation, _index| {
                        let _ = signal_tx.send(match (facility, operation) {
                            (Some(Facility::Sink), Some(Operation::Removed)) => Signal::SinkRemoved,
                            // Sink new/changed and server changes (default-sink switch) all mean
                            // "re-read the sinks' ports".
                            (Some(Facility::Sink), _) | (Some(Facility::Server), _) => Signal::SinkListChanged,
                            _ => return,
                        });
                    }
                })));

                let mut classifier = RouteClassifier::default();
                let mut subscribed = false;
                loop {
                    match mainloop.iterate(true) {
                        IterateResult::Success(_) => {}
                        // Somebody asked the main loop to stop — leave quietly.
                        IterateResult::Quit(_) => return,
                        IterateResult::Err(err) => {
                            let _ = init_tx.send(Err(RouteWatchError::NoAudioServer(format!("the audio-server connection failed: {err}"))));
                            return;
                        }
                    }

                    let mut rescan = false;
                    while let Ok(signal) = signal_rx.try_recv() {
                        match signal {
                            Signal::StateChanged => match context.get_state() {
                                State::Ready => {
                                    if !subscribed {
                                        // The `Operation` this returns is just the subscription
                                        // request's own ack — dropped here; the ack callback is
                                        // what reports failure.
                                        context.subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, {
                                            let signal_tx = signal_tx.clone();
                                            move |success| {
                                                if !success {
                                                    let _ = signal_tx.send(Signal::SubscribeFailed);
                                                }
                                            }
                                        });
                                        subscribed = true;
                                        // The first successful scan completes initialization.
                                        // Nothing is emitted for it: the classifier's initial
                                        // state is "unobserved", and both None-transitions that
                                        // could fire on it are handled at the classifier level.
                                        let _ = init_tx.send(Ok(()));
                                        rescan = true;
                                    }
                                }
                                // No audio server at all (empty sandbox) or the server died.
                                State::Failed | State::Terminated => {
                                    let _ = init_tx.send(Err(RouteWatchError::NoAudioServer("the audio-server connection failed".into())));
                                    return;
                                }
                                _ => {}
                            },
                            Signal::SinkListChanged => rescan = true,
                            Signal::SinkRemoved => {
                                if let Some(event) = classifier.on_sink_removed() {
                                    dispatch(&sender_for_thread, event);
                                }
                            }
                            Signal::SinkPorts { availability } => {
                                if let Some(event) = classifier.on_headphone_port(availability) {
                                    dispatch(&sender_for_thread, event);
                                }
                            }
                            Signal::SubscribeFailed => {
                                let _ = init_tx.send(Err(RouteWatchError::NoAudioServer("subscribing to sink events failed".into())));
                                return;
                            }
                        }
                    }

                    if rescan {
                        context.introspect().get_sink_info_list({
                            let signal_tx = signal_tx.clone();
                            move |result| {
                                if let ListResult::Item(info) = result {
                                    if let Some(availability) = headphone_availability(&info.ports) {
                                        let _ = signal_tx.send(Signal::SinkPorts { availability: Some(availability) });
                                    }
                                }
                            }
                        });
                    }
                }
            })
            .map_err(|err| RouteWatchError::NoAudioServer(format!("couldn't spawn the watcher thread: {err}")))?;

        match init_rx.recv_timeout(INIT_TIMEOUT) {
            Ok(Ok(())) => Ok(Self { sender }),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(RouteWatchError::NoAudioServer(format!(
                "the audio server didn't become ready within {}s",
                INIT_TIMEOUT.as_secs()
            ))),
        }
    }
}

fn dispatch(
    sender: &std::sync::Arc<std::sync::Mutex<Option<futures::channel::mpsc::UnboundedSender<RouteEvent>>>>,
    event: RouteEvent,
) {
    if let Some(tx) = sender.lock().expect("route-watch sender mutex").as_ref() {
        let _ = tx.unbounded_send(event);
    }
}

impl RouteWatcher for PulseRouteWatcher {
    fn start(&mut self, on_event: Box<dyn Fn(RouteEvent)>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<RouteEvent>();
        *self.sender.lock().expect("route-watch sender mutex") = Some(tx);
        // Runs on the GLib main loop (this is always called from it), so the callback may capture
        // non-`Send` GTK state freely — only `RouteEvent`s themselves cross the thread boundary.
        glib::spawn_future_local(async move {
            use futures::stream::StreamExt;
            while let Some(event) = rx.next().await {
                on_event(event);
            }
        });
    }
}

#[cfg(test)]
type FakeCallbackCell = std::cell::RefCell<Option<Box<dyn Fn(RouteEvent)>>>;

#[cfg(test)]
pub struct FakeRouteWatcher {
    callback: FakeCallbackCell,
}

#[cfg(test)]
impl FakeRouteWatcher {
    pub fn new() -> std::rc::Rc<Self> {
        std::rc::Rc::new(Self { callback: FakeCallbackCell::new(None) })
    }

    pub fn simulate(&self, event: RouteEvent) {
        if let Some(callback) = self.callback.borrow().as_ref() {
            callback(event);
        }
    }
}

#[cfg(test)]
impl RouteWatcher for FakeRouteWatcher {
    fn start(&mut self, on_event: Box<dyn Fn(RouteEvent)>) {
        *self.callback.borrow_mut() = Some(on_event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(available: PortAvailability) -> Option<PortAvailability> {
        Some(available)
    }

    #[test]
    fn classifier_silent_at_startup_whatever_the_headphone_state_is() {
        let mut classifier = RouteClassifier::default();
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::No)), None, "startup with headphones unplugged is the state of the world, not an event");

        // A fresh process with headphones already plugged in: `None -> yes` does emit Replugged
        // (deliberately — it's what a Bluetooth reconnection looks like after the sink vanished
        // and wiped the state), which is inert at true startup because nothing had been
        // auto-paused for it to undo. Assert the event to pin the design.
        let mut classifier = RouteClassifier::default();
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::Yes)), Some(RouteEvent::Replugged));
    }

    #[test]
    fn classifier_reports_unplug_on_yes_to_no() {
        let mut classifier = RouteClassifier::default();
        classifier.on_headphone_port(port(PortAvailability::Yes));
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::No)), Some(RouteEvent::Unplugged));
        // And it doesn't repeat itself: still absent is not a new event.
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::No)), None);
    }

    #[test]
    fn classifier_reports_replug_on_no_to_yes() {
        let mut classifier = RouteClassifier::default();
        classifier.on_headphone_port(port(PortAvailability::Yes));
        classifier.on_headphone_port(port(PortAvailability::No));
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::Yes)), Some(RouteEvent::Replugged));
    }

    #[test]
    fn classifier_treats_a_bt_reconnection_as_replug() {
        // Bluetooth: the sink disappears (wiping state) and returns later with its port available.
        let mut classifier = RouteClassifier::default();
        classifier.on_headphone_port(port(PortAvailability::Yes));
        assert_eq!(classifier.on_sink_removed(), Some(RouteEvent::Unplugged));
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::Yes)), Some(RouteEvent::Replugged));
    }

    #[test]
    fn classifier_ignores_unknown_availability_and_missing_ports() {
        let mut classifier = RouteClassifier::default();
        classifier.on_headphone_port(port(PortAvailability::Yes));
        // A USB DAC that can't do jack detection: no event, no state change.
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::Unknown)), None);
        assert_eq!(classifier.on_headphone_port(port(PortAvailability::No)), Some(RouteEvent::Unplugged), "the last *known* state is what the unplug is measured against");
        // A speaker-only sink (no headphone port at all): ignored.
        assert_eq!(classifier.on_headphone_port(None), None);
    }

    #[test]
    fn classifier_ignores_sink_removal_when_headphones_were_not_in_use() {
        let mut classifier = RouteClassifier::default();
        // Some other output (speakers, HDMI) going away, or removals before anything was observed.
        assert_eq!(classifier.on_sink_removed(), None);
        classifier.on_headphone_port(port(PortAvailability::No));
        assert_eq!(classifier.on_sink_removed(), None);
    }

    #[test]
    fn headphone_availability_finds_headphone_ports_and_ignores_others() {
        fn make_port<'a>(name: Option<&'a str>, available: PortAvailability) -> libpulse_binding::context::introspect::SinkPortInfo<'a> {
            libpulse_binding::context::introspect::SinkPortInfo {
                name: name.map(Into::into),
                description: None,
                priority: 0,
                available: match available {
                    PortAvailability::Unknown => libpulse_binding::def::PortAvailable::Unknown,
                    PortAvailability::Yes => libpulse_binding::def::PortAvailable::Yes,
                    PortAvailability::No => libpulse_binding::def::PortAvailable::No,
                },
            }
        }

        let ports = vec![
            make_port(Some("analog-output-speaker"), PortAvailability::Yes),
            make_port(Some("analog-output-headphones"), PortAvailability::No),
        ];
        assert_eq!(headphone_availability(&ports), Some(PortAvailability::No), "the headphone port is the one that counts, not the first port");

        let speakers_only = vec![make_port(Some("analog-output-speaker"), PortAvailability::Yes)];
        assert_eq!(headphone_availability(&speakers_only), None);
    }

    /// Registers against a *real* audio server — only checkable when one is reachable. This
    /// sandbox has no PulseAudio/PipeWire socket at all, so `PulseRouteWatcher::new()` correctly
    /// returns `Err` rather than panicking — exactly the graceful-degradation path exercised for
    /// real. Run explicitly via `cargo test -p abs-player -- --ignored route_watch::tests::new`
    /// once a running audio server is confirmed present.
    #[test]
    #[ignore]
    fn new_connects_and_initializes_against_a_real_audio_server() {
        let mut watcher = PulseRouteWatcher::new().expect("connect to the audio server");
        watcher.start(Box::new(|_event| {}));
    }
}
