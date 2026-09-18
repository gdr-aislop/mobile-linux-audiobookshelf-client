//! The Settings destination — per `docs/design/ui-spec.md`'s "Settings" section, an
//! `AdwPreferencesPage` under the tab bar. **Partially built**: only the Playback group exists so
//! far, holding the headphone-behavior switches that `abs_player::route_watch`'s consumers act
//! on. The remaining spec'd groups (Account, Servers, Appearance, About) and the rest of Playback
//! (default speed, skip intervals, sleep-timer default) are yet to be built — each arrives with
//! its own feature, since every row is live wiring rather than decoration.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::settings::PlaybackSettings;

use crate::player::PlayerController;

pub struct SettingsScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    pub hooks: SettingsHooks,
}

#[cfg(test)]
pub struct SettingsHooks {
    pub pause_on_unplug_switch: gtk4::Switch,
    pub resume_on_replug_switch: gtk4::Switch,
}

pub fn build(pool: SqlitePool, controller: PlayerController, playback_settings: PlaybackSettings) -> SettingsScreen {
    // The one shared copy of the settings this page edits: each switch mutates its own field,
    // then the *whole* struct is persisted (the kv store round-trips everything, so partial
    // writes would silently reset the other fields to their pre-edit values).
    let settings = Rc::new(RefCell::new(playback_settings));
    // Save serialization state — see `persist` for why two saves must never run concurrently.
    let pending_save: Rc<RefCell<Option<PlaybackSettings>>> = Rc::new(RefCell::new(None));
    let writer_running: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Settings", "")));

    let page = adw::PreferencesPage::new();

    let playback_group = adw::PreferencesGroup::new();
    playback_group.set_title("Playback");

    let pause_on_unplug_switch = gtk4::Switch::new();
    pause_on_unplug_switch.set_valign(gtk4::Align::Center);
    // Both properties: `state` is what user toggles drive (via `state-set`), `active` is what
    // the next `activate` toggles *from* — initializing only one of them would make the first
    // activation jump to a stale value.
    pause_on_unplug_switch.set_state(playback_settings.pause_on_headphone_unplug);
    pause_on_unplug_switch.set_active(playback_settings.pause_on_headphone_unplug);
    let pause_row = adw::ActionRow::builder()
        .title("Pause when headphones disconnect")
        .subtitle("Wired headphones unplugged, or Bluetooth audio lost")
        .build();
    pause_row.add_suffix(&pause_on_unplug_switch);
    pause_row.set_activatable_widget(Some(&pause_on_unplug_switch));
    playback_group.add(&pause_row);

    let resume_on_replug_switch = gtk4::Switch::new();
    resume_on_replug_switch.set_valign(gtk4::Align::Center);
    resume_on_replug_switch.set_state(playback_settings.resume_on_headphone_replug);
    resume_on_replug_switch.set_active(playback_settings.resume_on_headphone_replug);
    resume_on_replug_switch.set_sensitive(playback_settings.pause_on_headphone_unplug);
    let resume_row = adw::ActionRow::builder()
        .title("Resume when headphones reconnect")
        .subtitle("Only undoes a pause caused by disconnecting — never a manual pause or a call")
        .build();
    resume_row.add_suffix(&resume_on_replug_switch);
    resume_row.set_activatable_widget(Some(&resume_on_replug_switch));
    playback_group.add(&resume_row);

    // A replug can only ever lift an unplug pause, so with pause-on-unplug off, resume has
    // nothing to act on — reflecting that in the UI beats letting it silently do nothing.
    pause_on_unplug_switch.connect_state_set({
        let resume_on_replug_switch = resume_on_replug_switch.clone();
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        move |_, state| {
            settings.borrow_mut().pause_on_headphone_unplug = state;
            persist(&pool, &pending_save, &writer_running, &settings, &controller);
            resume_on_replug_switch.set_sensitive(state);
            glib::signal::Propagation::Proceed
        }
    });
    resume_on_replug_switch.connect_state_set({
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        move |_, state| {
            settings.borrow_mut().resume_on_headphone_replug = state;
            persist(&pool, &pending_save, &writer_running, &settings, &controller);
            glib::signal::Propagation::Proceed
        }
    });

    page.add(&playback_group);

    let root = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    root.append(&header);
    root.append(&page);

    SettingsScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: SettingsHooks { pause_on_unplug_switch, resume_on_replug_switch },
    }
}

/// Fire-and-forget persistence on the shared Tokio runtime — same shape as every other GTK
/// signal handler that touches the database (`PlayerController`'s progress writes, Welcome's
/// Connect button): capture everything up front, spawn, log on failure. Also applies the new
/// values to the live controller immediately, so a toggle takes effect without an app restart.
///
/// Saves are serialized through `pending_save`/`writer_running` — one snapshot slot plus at most
/// one in-flight writer task. Two concurrent `save_playback_settings` calls would interleave
/// their per-key `kv::set`s (the pool allows multiple connections), so a stale snapshot's
/// `pause_on_headphone_unplug=true` could land *after* a newer snapshot's `false` and win —
/// caught live by the gtk_fast scenario: the resume toggle's save overracing the pause toggle's
/// left the just-toggled value silently un-persisted. Draining the slot until empty also
/// coalesces a rapid toggle burst into at most one save per still-queued snapshot.
fn persist(
    pool: &SqlitePool,
    pending_save: &Rc<RefCell<Option<PlaybackSettings>>>,
    writer_running: &Rc<Cell<bool>>,
    settings: &RefCell<PlaybackSettings>,
    controller: &PlayerController,
) {
    let snapshot = *settings.borrow();
    controller.set_headphone_behavior(snapshot.pause_on_headphone_unplug, snapshot.resume_on_headphone_replug);
    *pending_save.borrow_mut() = Some(snapshot);
    if writer_running.get() {
        return;
    }
    writer_running.set(true);
    let pool = pool.clone();
    let pending_save = pending_save.clone();
    let writer_running = writer_running.clone();
    glib::spawn_future_local(async move {
        loop {
            let Some(snapshot) = pending_save.borrow_mut().take() else { break };
            if let Err(err) = abs_core::settings::save_playback_settings(&pool, &snapshot).await {
                tracing::warn!(%err, "couldn't save playback settings");
            }
        }
        writer_running.set(false);
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::build;
    use adw::prelude::*;
    use std::time::Duration;

    use crate::player::tests::test_backend;
    use crate::test_support::pump_until;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let screen = build(pool.clone(), controller.clone(), abs_core::settings::PlaybackSettings::default());

        assert!(screen.hooks.pause_on_unplug_switch.state(), "pause-on-unplug defaults to on");
        assert!(!screen.hooks.resume_on_replug_switch.state(), "resume-on-replug defaults to off");
        assert!(screen.hooks.resume_on_replug_switch.is_sensitive(), "resume is offered while pause-on-unplug is on");

        // A real click delivers the `state-set` signal with the new state (GTK's own
        // `activate` doesn't toggle plain switches in this GTK version), running the app's
        // handler — live-controller update plus persistence — before the class handler applies
        // the state. Emit exactly that, on both switches.
        let _: bool = screen.hooks.resume_on_replug_switch.emit_by_name("state-set", &[&true]);
        let _: bool = screen.hooks.pause_on_unplug_switch.emit_by_name("state-set", &[&false]);
        assert!(!screen.hooks.pause_on_unplug_switch.state());
        assert!(screen.hooks.resume_on_replug_switch.state());
        assert!(!screen.hooks.resume_on_replug_switch.is_sensitive(), "with pause-on-unplug off, resume has nothing to act on");
        pump_until(|| false, Duration::from_millis(300));
        assert_eq!(controller.headphone_behavior(), (false, true), "switch activation must reach the live controller");

        let (pause_saved, resume_saved) = runtime.block_on(async {
            let settings = abs_core::settings::load_playback_settings(&pool).await.unwrap();
            (settings.pause_on_headphone_unplug, settings.resume_on_headphone_replug)
        });
        assert!(!pause_saved, "toggling pause-on-unplug must persist");
        assert!(resume_saved, "toggling resume-on-replug must persist");
    }
}
