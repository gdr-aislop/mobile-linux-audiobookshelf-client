//! MPRIS2 (`org.mpris.MediaPlayer2`) system media integration —
//! `docs/design/ui-spec.md`'s "System media integration" section. This lives in `abs-player`
//! rather than `app` because it's real OS I/O (a D-Bus session), the same category this crate
//! already owns for GStreamer — not a GTK widget-toolkit concern. `gio`/`glib` are direct
//! dependencies here (already present transitively via `gtk4`/`adw`), not `zbus`: no new external
//! crate, and object registration is exactly `gio::DBusConnection`'s own API.
//!
//! This module never imports anything from `app` — [`register`] takes a plain trait object
//! (`Rc<dyn MprisCommands>`) supplied by the caller and calls back into it; `app/src/player.rs`
//! is the only place that ties `MprisCommands` to a real `PlayerController`. That's the whole
//! dependency-direction story: `app` depends on `abs-player`, never the reverse.
//!
//! `Next`/`Previous` are named per the MPRIS spec but are wired to skip-forward/back-N-seconds,
//! never a literal track change — this app has no playlist concept, per
//! `docs/design/ui-spec.md`'s explicit mapping.

use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;

const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const MEDIA_PLAYER2_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// A fixed dummy track id — this app has no MPRIS `TrackList` support (no playlist concept), so
/// there's nothing meaningful to distinguish one track's id from another's. Real MPRIS clients
/// (lock-screen widgets, GNOME Shell's media controls) only use this as an opaque identifier, not
/// as a lookup key, so a fixed value is spec-compliant even though it's the same for every item.
const TRACK_ID_PATH: &str = "/org/mpris/MediaPlayer2/Track/1";

const INTROSPECTION_XML: &str = r#"
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek">
      <arg direction="in" name="Offset" type="x"/>
    </method>
    <method name="SetPosition">
      <arg direction="in" name="TrackId" type="o"/>
      <arg direction="in" name="Position" type="x"/>
    </method>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="Rate" type="d" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn as_str(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: Option<String>,
    pub length_micros: i64,
    /// A `file://` URI — MPRIS's `mpris:artUrl` accepts any URI, and the cover is always a local
    /// cache file, never served directly from a remote URL.
    pub art_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlayerState {
    pub status: PlaybackStatus,
    pub metadata: TrackMetadata,
    pub position_micros: i64,
    pub rate: f64,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self { status: PlaybackStatus::Stopped, metadata: TrackMetadata::default(), position_micros: 0, rate: 1.0 }
    }
}

/// Commands an MPRIS client (lock screen, GNOME Shell's media widget, a Bluetooth headset) can
/// invoke. Implemented by `app/src/player.rs`'s bridge over the real `PlayerController` — this
/// trait is the whole boundary, so `abs-player` never needs to know what a `PlayerController` is.
pub trait MprisCommands {
    fn play_pause(&self);
    fn play(&self);
    fn pause(&self);
    /// A relative seek, in microseconds (matches MPRIS's `Seek` method exactly) — positive skips
    /// forward, negative skips back.
    fn seek(&self, offset_micros: i64);
    /// An absolute seek, in microseconds (matches MPRIS's `SetPosition` method).
    fn set_position(&self, position_micros: i64);
    /// Mapped to skip-forward-N-seconds, never a literal track change (see module docs).
    fn next(&self);
    /// Mapped to skip-back-N-seconds, never a literal track change (see module docs).
    fn previous(&self);
}

#[derive(Debug, thiserror::Error)]
pub enum MprisError {
    #[error("couldn't reach the D-Bus session bus: {0}")]
    NoSessionBus(glib::Error),
    #[error("couldn't parse MPRIS introspection XML: {0}")]
    InvalidIntrospection(glib::Error),
    #[error("MPRIS introspection XML is missing the {0} interface")]
    MissingInterface(&'static str),
    #[error("couldn't register the MPRIS object: {0}")]
    RegistrationFailed(glib::Error),
}

/// Registers this app as an MPRIS2 media player on the session bus. Returns `Err` rather than
/// panicking if no session bus is reachable (headless CI, a sandboxed environment with no D-Bus) —
/// callers should log a warning and continue; MPRIS absence must never be fatal to playback.
pub fn register(app_name: &str, commands: Rc<dyn MprisCommands>) -> Result<MprisHandle, MprisError> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(MprisError::NoSessionBus)?;

    let node_info = gio::DBusNodeInfo::for_xml(INTROSPECTION_XML).map_err(MprisError::InvalidIntrospection)?;
    let media_player2_info = node_info.lookup_interface(MEDIA_PLAYER2_IFACE).ok_or(MprisError::MissingInterface(MEDIA_PLAYER2_IFACE))?;
    let player_info = node_info.lookup_interface(PLAYER_IFACE).ok_or(MprisError::MissingInterface(PLAYER_IFACE))?;

    let state = Rc::new(RefCell::new(PlayerState::default()));
    let app_name = app_name.to_string();

    let media_player2_registration = connection
        .register_object(OBJECT_PATH, &media_player2_info)
        .method_call(|_conn, _sender, _path, _iface, method, _params, invocation| {
            // `Raise`/`Quit` have no meaningful behavior for a phone player with no separate
            // "bring to front" concept distinct from just switching apps — acknowledge, no-op.
            let _ = method;
            invocation.return_value(None);
        })
        .property({
            let app_name = app_name.clone();
            move |_conn, _sender, _path, _iface, property| media_player2_property(property, &app_name)
        })
        .build()
        .map_err(MprisError::RegistrationFailed)?;

    let player_registration = connection
        .register_object(OBJECT_PATH, &player_info)
        .method_call({
            let commands = commands.clone();
            move |_conn, _sender, _path, _iface, method, params, invocation| {
                match dispatch_player_method(method, &params, commands.as_ref()) {
                    Ok(reply) => invocation.return_value(reply.as_ref()),
                    Err(err) => invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", &err.to_string()),
                }
            }
        })
        .property({
            let state = state.clone();
            move |_conn, _sender, _path, _iface, property| player_property(property, &state.borrow())
        })
        .build()
        .map_err(MprisError::RegistrationFailed)?;

    // Owning the well-known name is best-effort: a failure here (e.g. the name is already taken
    // by another instance of this app) shouldn't tear down the object registrations above —
    // clients that already know the object path can still reach it directly.
    let _owner_id = gio::bus_own_name_on_connection(
        &connection,
        &format!("org.mpris.MediaPlayer2.{app_name}"),
        gio::BusNameOwnerFlags::NONE,
        |_conn, _name| {},
        |_conn, name| tracing::warn!(name, "couldn't own the MPRIS well-known bus name"),
    );

    Ok(MprisHandle { connection, state, _media_player2_registration: media_player2_registration, _player_registration: player_registration })
}

pub struct MprisHandle {
    connection: gio::DBusConnection,
    state: Rc<RefCell<PlayerState>>,
    _media_player2_registration: gio::RegistrationId,
    _player_registration: gio::RegistrationId,
}

impl MprisHandle {
    /// Replaces the whole player state and notifies any listening client via the standard
    /// `org.freedesktop.DBus.Properties.PropertiesChanged` signal — this is what keeps a
    /// lock-screen media card in sync with real playback.
    pub fn update(&self, new_state: PlayerState) {
        let previous = self.state.borrow().clone();
        *self.state.borrow_mut() = new_state.clone();

        if !player_state_changed(&previous, &new_state) {
            return;
        }

        let changed = glib::VariantDict::new(None);
        changed.insert("PlaybackStatus", new_state.status.as_str());
        changed.insert("Metadata", metadata_variant(&new_state.metadata));
        changed.insert("Rate", new_state.rate);
        // `Position` is deliberately excluded from `PropertiesChanged` per the MPRIS spec itself
        // ("Position ... may be changed without notification") — clients are expected to poll
        // `Position` (or seek from `Seeked`), not treat it as a properties-changed field.

        let params = glib::Variant::tuple_from_iter([
            PLAYER_IFACE.to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        if let Err(err) = self.connection.emit_signal(None, OBJECT_PATH, PROPERTIES_IFACE, "PropertiesChanged", Some(&params)) {
            tracing::warn!(%err, "couldn't emit MPRIS PropertiesChanged");
        }
    }
}

/// Whether any MPRIS-relevant field changed. `position_micros` is deliberately excluded —
/// see the comment in `MprisHandle::update` on why `Position` never triggers a signal.
fn player_state_changed(previous: &PlayerState, new: &PlayerState) -> bool {
    previous.status != new.status || previous.metadata != new.metadata || previous.rate != new.rate
}

fn media_player2_property(property: &str, app_name: &str) -> glib::Variant {
    match property {
        "CanQuit" => false.to_variant(),
        "CanRaise" => false.to_variant(),
        "HasTrackList" => false.to_variant(),
        "Identity" => app_name.to_variant(),
        "SupportedUriSchemes" => Vec::<String>::new().to_variant(),
        "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
        _ => false.to_variant(),
    }
}

fn player_property(property: &str, state: &PlayerState) -> glib::Variant {
    match property {
        "PlaybackStatus" => state.status.as_str().to_variant(),
        "Rate" => state.rate.to_variant(),
        "Metadata" => metadata_variant(&state.metadata),
        "Position" => state.position_micros.to_variant(),
        "MinimumRate" => 0.8_f64.to_variant(),
        "MaximumRate" => 3.0_f64.to_variant(),
        "CanGoNext" | "CanGoPrevious" | "CanPlay" | "CanPause" | "CanSeek" | "CanControl" => true.to_variant(),
        _ => false.to_variant(),
    }
}

fn metadata_variant(metadata: &TrackMetadata) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    dict.insert("mpris:trackid", glib::variant::ObjectPath::try_from(TRACK_ID_PATH).expect("a fixed, valid object path"));
    dict.insert("mpris:length", metadata.length_micros);
    dict.insert("xesam:title", metadata.title.as_str());
    if let Some(artist) = &metadata.artist {
        dict.insert("xesam:artist", vec![artist.clone()]);
    }
    if let Some(art_url) = &metadata.art_url {
        dict.insert("mpris:artUrl", art_url.as_str());
    }
    dict.end()
}

/// Factored out of the D-Bus method-call closure so it's unit-testable against a fake
/// [`MprisCommands`] with no real D-Bus connection at all — the main fast-test surface for this
/// module (a real bus round-trip can only be smoke-tested, gated on whatever D-Bus tooling exists
/// in a given build/test environment).
fn dispatch_player_method(method: &str, params: &glib::Variant, commands: &dyn MprisCommands) -> Result<Option<glib::Variant>, glib::Error> {
    match method {
        "PlayPause" => {
            commands.play_pause();
            Ok(None)
        }
        "Play" => {
            commands.play();
            Ok(None)
        }
        "Pause" | "Stop" => {
            // No separate "stopped" state exists in this app (see `PlaybackStatus`'s three
            // variants — `Stopped` is only ever reported, never entered from a running session);
            // MPRIS's `Stop` is treated the same as `Pause`.
            commands.pause();
            Ok(None)
        }
        "Next" => {
            commands.next();
            Ok(None)
        }
        "Previous" => {
            commands.previous();
            Ok(None)
        }
        "Seek" => {
            let offset: i64 = params
                .child_value(0)
                .get()
                .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::InvalidArgument, "Seek expects an int64 offset"))?;
            commands.seek(offset);
            Ok(None)
        }
        "SetPosition" => {
            let position: i64 = params
                .child_value(1)
                .get()
                .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::InvalidArgument, "SetPosition expects an int64 position"))?;
            commands.set_position(position);
            Ok(None)
        }
        other => Err(glib::Error::new(gio::IOErrorEnum::NotSupported, &format!("unknown MPRIS method {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct FakeCommands {
        play_pause_calls: Cell<u32>,
        play_calls: Cell<u32>,
        pause_calls: Cell<u32>,
        seek_offset: Cell<Option<i64>>,
        set_position: Cell<Option<i64>>,
        next_calls: Cell<u32>,
        previous_calls: Cell<u32>,
    }

    impl MprisCommands for FakeCommands {
        fn play_pause(&self) {
            self.play_pause_calls.set(self.play_pause_calls.get() + 1);
        }
        fn play(&self) {
            self.play_calls.set(self.play_calls.get() + 1);
        }
        fn pause(&self) {
            self.pause_calls.set(self.pause_calls.get() + 1);
        }
        fn seek(&self, offset_micros: i64) {
            self.seek_offset.set(Some(offset_micros));
        }
        fn set_position(&self, position_micros: i64) {
            self.set_position.set(Some(position_micros));
        }
        fn next(&self) {
            self.next_calls.set(self.next_calls.get() + 1);
        }
        fn previous(&self) {
            self.previous_calls.set(self.previous_calls.get() + 1);
        }
    }

    #[test]
    fn play_pause_calls_through() {
        let commands = FakeCommands::default();
        dispatch_player_method("PlayPause", &().to_variant(), &commands).unwrap();
        assert_eq!(commands.play_pause_calls.get(), 1);
    }

    #[test]
    fn stop_is_treated_as_pause() {
        let commands = FakeCommands::default();
        dispatch_player_method("Stop", &().to_variant(), &commands).unwrap();
        assert_eq!(commands.pause_calls.get(), 1);
    }

    #[test]
    fn next_and_previous_call_through_without_changing_tracks() {
        let commands = FakeCommands::default();
        dispatch_player_method("Next", &().to_variant(), &commands).unwrap();
        dispatch_player_method("Previous", &().to_variant(), &commands).unwrap();
        assert_eq!(commands.next_calls.get(), 1);
        assert_eq!(commands.previous_calls.get(), 1);
    }

    #[test]
    fn seek_extracts_the_offset() {
        let commands = FakeCommands::default();
        let params = glib::Variant::tuple_from_iter([(-5_000_000_i64).to_variant()]);
        dispatch_player_method("Seek", &params, &commands).unwrap();
        assert_eq!(commands.seek_offset.get(), Some(-5_000_000));
    }

    #[test]
    fn set_position_extracts_the_position_not_the_track_id() {
        let commands = FakeCommands::default();
        let params = glib::Variant::tuple_from_iter([
            glib::variant::ObjectPath::try_from(TRACK_ID_PATH).unwrap().to_variant(),
            42_000_000_i64.to_variant(),
        ]);
        dispatch_player_method("SetPosition", &params, &commands).unwrap();
        assert_eq!(commands.set_position.get(), Some(42_000_000));
    }

    #[test]
    fn unknown_method_is_an_error_not_a_panic() {
        let commands = FakeCommands::default();
        assert!(dispatch_player_method("SomethingUnsupported", &().to_variant(), &commands).is_err());
    }

    #[test]
    fn introspection_xml_parses_and_has_both_interfaces() {
        let node_info = gio::DBusNodeInfo::for_xml(INTROSPECTION_XML).expect("valid introspection XML");
        assert!(node_info.lookup_interface(MEDIA_PLAYER2_IFACE).is_some());
        assert!(node_info.lookup_interface(PLAYER_IFACE).is_some());
    }

    #[test]
    fn player_property_reports_current_state() {
        let state = PlayerState {
            status: PlaybackStatus::Playing,
            metadata: TrackMetadata { title: "A Book".to_string(), artist: Some("An Author".to_string()), length_micros: 1_000_000, art_url: None },
            position_micros: 500_000,
            rate: 1.5,
        };
        assert_eq!(player_property("PlaybackStatus", &state).str(), Some("Playing"));
        assert_eq!(player_property("Position", &state).get::<i64>(), Some(500_000));
        assert_eq!(player_property("Rate", &state).get::<f64>(), Some(1.5));
    }

    /// Registers against a *real* session bus — only checkable when one is reachable, which this
    /// sandbox doesn't provide by default (`register`'s own graceful `Err` on a missing bus is
    /// what fast tests above exercise indirectly by never needing a bus at all). Run explicitly
    /// via `dbus-run-session -- cargo test -p abs-player -- --ignored mpris::tests::register`
    /// once `dbus-run-session` is confirmed present (checked during implementation: it is, in
    /// this build environment, though real hardware verification of the lock-screen card itself
    /// still requires GNOME Shell/phosh).
    #[test]
    #[ignore]
    fn register_succeeds_against_a_real_session_bus() {
        let commands = Rc::new(FakeCommands::default());
        let handle = register("AbsPlayerLiveTest", commands).expect("register should succeed with a real session bus reachable");
        handle.update(PlayerState {
            status: PlaybackStatus::Playing,
            metadata: TrackMetadata { title: "Live Test".to_string(), artist: None, length_micros: 1, art_url: None },
            position_micros: 0,
            rate: 1.0,
        });
    }

    #[test]
    fn metadata_variant_includes_title_and_artist() {
        let metadata = TrackMetadata { title: "A Book".to_string(), artist: Some("An Author".to_string()), length_micros: 42, art_url: None };
        let variant = metadata_variant(&metadata);
        let dict = glib::VariantDict::new(Some(&variant));
        assert_eq!(dict.lookup_value("xesam:title", None).and_then(|v| v.str().map(str::to_string)), Some("A Book".to_string()));
        assert_eq!(dict.lookup_value("mpris:length", None).and_then(|v| v.get::<i64>()), Some(42));
    }

    #[test]
    fn player_state_changed_is_false_when_only_position_differs() {
        let previous = PlayerState { status: PlaybackStatus::Playing, position_micros: 0, ..PlayerState::default() };
        let new = PlayerState { status: PlaybackStatus::Playing, position_micros: 250_000, ..PlayerState::default() };
        assert!(!player_state_changed(&previous, &new));
    }

    #[test]
    fn player_state_changed_is_true_when_status_or_metadata_or_rate_differs() {
        let base = PlayerState::default();

        let status_changed = PlayerState { status: PlaybackStatus::Playing, ..base.clone() };
        assert!(player_state_changed(&base, &status_changed));

        let metadata_changed =
            PlayerState { metadata: TrackMetadata { title: "New Title".to_string(), ..TrackMetadata::default() }, ..base.clone() };
        assert!(player_state_changed(&base, &metadata_changed));

        let rate_changed = PlayerState { rate: 1.5, ..base.clone() };
        assert!(player_state_changed(&base, &rate_changed));
    }
}
