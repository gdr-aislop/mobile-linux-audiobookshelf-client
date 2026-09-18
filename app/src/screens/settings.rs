//! The Settings destination — per `docs/design/ui-spec.md`'s "Settings" section, an
//! `AdwPreferencesPage` under the tab bar. **Partially built**: the Playback group (headphone
//! switches, default speed, skip intervals, Wi-Fi-only downloads), the Appearance group (Theme)
//! and the About row are real; Account, Servers, the Connection page and Playback's
//! sleep-timer-default row are yet to come — each arrives with its own feature, since every row
//! is live wiring (session management, popover integration) rather than decoration.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::playback::SPEED_PRESETS;
use abs_core::settings::{PlaybackSettings, Theme};

use crate::downloads::DownloadManager;
use crate::player::PlayerController;

/// Skip-interval choices (seconds) for Settings → Playback — the full player's transport buttons,
/// arrow keys and MPRIS next/previous all step by one of these.
const SKIP_CHOICES: [i64; 7] = [5, 10, 15, 20, 30, 45, 60];

pub struct SettingsScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    pub hooks: SettingsHooks,
}

#[cfg(test)]
pub struct SettingsHooks {
    pub pause_on_unplug_switch: gtk4::Switch,
    pub resume_on_replug_switch: gtk4::Switch,
    pub default_speed_row: adw::ComboRow,
    pub skip_back_row: adw::ComboRow,
    pub skip_forward_row: adw::ComboRow,
    pub wifi_only_switch: gtk4::Switch,
    pub theme_row: adw::ComboRow,
    pub about_row: adw::ActionRow,
}

pub fn build(
    pool: SqlitePool,
    controller: PlayerController,
    download_manager: DownloadManager,
    playback_settings: PlaybackSettings,
    theme: Theme,
    window: adw::ApplicationWindow,
) -> SettingsScreen {
    // The one shared copy of the settings this page edits: each row mutates its own field, then
    // the *whole* struct is persisted (the kv store round-trips everything, so partial writes
    // would silently reset the other fields to their pre-edit values). The theme is stored under
    // its own single key (`save_theme`) and deliberately kept out of this struct — it has no
    // whole-struct overwrite hazard.
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
        let download_manager = download_manager.clone();
        move |_, state| {
            settings.borrow_mut().pause_on_headphone_unplug = state;
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
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
        let download_manager = download_manager.clone();
        move |_, state| {
            settings.borrow_mut().resume_on_headphone_replug = state;
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
            glib::signal::Propagation::Proceed
        }
    });

    // Start-of-session speed and skip intervals — the same presets/choices the full player's
    // speed popover and transport buttons use (one shared constant each, so they can't drift).
    // `set_selected` fires the same `notify::selected` handler user choice does, so the initial
    // value goes through the identical path; the handlers are connected after that.
    let default_speed_row = combo_row("Default speed", "Speed every book starts at", &speed_labels());
    default_speed_row.set_selected(speed_index(playback_settings.default_speed));
    playback_group.add(&default_speed_row);
    let skip_back_row = combo_row("Skip back", "Seconds the back button and ← key jump", &skip_labels());
    skip_back_row.set_selected(skip_index(playback_settings.skip_back_seconds, 15));
    playback_group.add(&skip_back_row);
    let skip_forward_row = combo_row("Skip forward", "Seconds the forward button and → key jump", &skip_labels());
    skip_forward_row.set_selected(skip_index(playback_settings.skip_forward_seconds, 30));
    playback_group.add(&skip_forward_row);

    let wifi_only_switch = gtk4::Switch::new();
    wifi_only_switch.set_valign(gtk4::Align::Center);
    wifi_only_switch.set_state(playback_settings.wifi_only_downloads);
    wifi_only_switch.set_active(playback_settings.wifi_only_downloads);
    let wifi_only_row = adw::ActionRow::builder()
        .title("Wi-Fi only downloads")
        .subtitle("Don't start downloads on metered connections")
        .build();
    wifi_only_row.add_suffix(&wifi_only_switch);
    wifi_only_row.set_activatable_widget(Some(&wifi_only_switch));
    playback_group.add(&wifi_only_row);

    default_speed_row.connect_selected_notify({
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        let download_manager = download_manager.clone();
        move |row| {
            settings.borrow_mut().default_speed = SPEED_PRESETS[row.selected() as usize];
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
        }
    });
    skip_back_row.connect_selected_notify({
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        let download_manager = download_manager.clone();
        move |row| {
            settings.borrow_mut().skip_back_seconds = SKIP_CHOICES[row.selected() as usize];
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
        }
    });
    skip_forward_row.connect_selected_notify({
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        let download_manager = download_manager.clone();
        move |row| {
            settings.borrow_mut().skip_forward_seconds = SKIP_CHOICES[row.selected() as usize];
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
        }
    });
    wifi_only_switch.connect_state_set({
        let settings = settings.clone();
        let pending_save = pending_save.clone();
        let writer_running = writer_running.clone();
        let controller = controller.clone();
        let pool = pool.clone();
        let download_manager = download_manager.clone();
        move |_, state| {
            settings.borrow_mut().wifi_only_downloads = state;
            persist(&pool, &pending_save, &writer_running, &settings, &controller, &download_manager);
            glib::signal::Propagation::Proceed
        }
    });

    page.add(&playback_group);

    let appearance_group = adw::PreferencesGroup::new();
    appearance_group.set_title("Appearance");
    let theme_row = combo_row("Theme", "", &["System", "Light", "Dark"].map(String::from));
    theme_row.set_selected(theme_index(theme));
    appearance_group.add(&theme_row);
    theme_row.connect_selected_notify({
        let pool = pool.clone();
        move |row| {
            let theme = theme_from_index(row.selected());
            crate::application::apply_theme(theme);
            let pool = pool.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = abs_core::settings::save_theme(&pool, theme).await {
                    tracing::warn!(%err, "couldn't save theme setting");
                }
            });
        }
    });
    page.add(&appearance_group);

    let about_group = adw::PreferencesGroup::new();
    about_group.set_title("About");
    let about_row = adw::ActionRow::builder()
        .title("About Audiobookshelf")
        .subtitle(format!("Version {}", env!("CARGO_PKG_VERSION")))
        .build();
    about_row.add_suffix(&gtk4::Image::from_icon_name("go-next-symbolic"));
    about_row.set_activatable(true);
    about_row.connect_activated({
        let window = window.clone();
        move |_| {
            // `AdwAboutWindow`, not `AdwAboutDialog` — the dialog needs libadwaita 1.5+, out of
            // reach of this crate's v1.2 ceiling (see `app/Cargo.toml`).
            adw::AboutWindow::builder()
                .application_name("Audiobookshelf")
                .version(env!("CARGO_PKG_VERSION"))
                .website("https://github.com/gdr-aislop/mobile-linux-audiobookshelf-client")
                .license_type(gtk4::License::Gpl30)
                .transient_for(&window)
                .modal(true)
                .build()
                .present();
        }
    });
    about_group.add(&about_row);
    page.add(&about_group);

    let root = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    root.append(&header);
    root.append(&page);

    SettingsScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: SettingsHooks {
            pause_on_unplug_switch,
            resume_on_replug_switch,
            default_speed_row,
            skip_back_row,
            skip_forward_row,
            wifi_only_switch,
            theme_row,
            about_row,
        },
    }
}

/// An `AdwComboRow` over a plain list of option labels — the string expression is what makes the
/// row display the selected option's text in its trailing slot.
fn combo_row(title: &str, subtitle: &str, options: &[String]) -> adw::ComboRow {
    let model = gtk4::StringList::new(&[]);
    for option in options {
        model.append(option);
    }
    let row = adw::ComboRow::builder().title(title).subtitle(subtitle).model(&model).build();
    row.set_expression(Some(gtk4::StringObject::this_expression("string")));
    row
}

fn speed_labels() -> Vec<String> {
    SPEED_PRESETS.iter().map(|speed| crate::screens::player::format_speed(*speed)).collect()
}

fn skip_labels() -> Vec<String> {
    SKIP_CHOICES.iter().map(|seconds| format!("{seconds} sec")).collect()
}

/// Index of a speed within `SPEED_PRESETS`, falling back to the default speed's position when the
/// stored value isn't a preset (e.g. after a settings-format change) — the row must always show
/// something a preset means.
fn speed_index(speed: f64) -> u32 {
    SPEED_PRESETS
        .iter()
        .position(|preset| (*preset - speed).abs() < f64::EPSILON)
        .unwrap_or_else(|| SPEED_PRESETS.iter().position(|preset| (*preset - abs_core::playback::DEFAULT_SPEED).abs() < f64::EPSILON).unwrap_or(0)) as u32
}

/// Index of an interval within `SKIP_CHOICES`, falling back to `fallback`'s position when the
/// stored value isn't a choice — same reasoning as `speed_index`.
fn skip_index(seconds: i64, fallback: i64) -> u32 {
    SKIP_CHOICES
        .iter()
        .position(|choice| *choice == seconds)
        .unwrap_or_else(|| SKIP_CHOICES.iter().position(|choice| *choice == fallback).unwrap_or(0)) as u32
}

fn theme_index(theme: Theme) -> u32 {
    match theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

fn theme_from_index(index: u32) -> Theme {
    match index {
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => Theme::System,
    }
}

/// Fire-and-forget persistence on the shared Tokio runtime — same shape as every other GTK
/// signal handler that touches the database (`PlayerController`'s progress writes, Welcome's
/// Connect button): capture everything up front, spawn, log on failure. Also applies the new
/// values to the live controller and download manager immediately, so a change takes effect
/// without an app restart.
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
    download_manager: &DownloadManager,
) {
    let snapshot = *settings.borrow();
    controller.set_headphone_behavior(snapshot.pause_on_headphone_unplug, snapshot.resume_on_headphone_replug);
    controller.set_playback_config(
        snapshot.default_speed,
        snapshot.skip_back_seconds as f64,
        snapshot.skip_forward_seconds as f64,
    );
    download_manager.set_wifi_only(snapshot.wifi_only_downloads);
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

    fn test_download_manager(pool: sqlx::SqlitePool) -> crate::downloads::DownloadManager {
        crate::downloads::DownloadManager::new(
            pool,
            crate::test_support::test_paths(),
            Box::new(abs_player::network_watch::UnknownNetworkMonitor),
            true,
        )
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let screen = build(
            pool.clone(),
            controller.clone(),
            test_download_manager(pool.clone()),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            adw::ApplicationWindow::builder().build(),
        );

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

    /// The rows added with the Playback/Appearance/About groups: the combos and Wi-Fi switch
    /// start from the settings the shell was built with, reach the live controller/manager on
    /// change, persist (including the theme's own key), and the About row opens an about window.
    pub(crate) fn run_playback_defaults_theme_and_about(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let download_manager = test_download_manager(pool.clone());
        let screen = build(
            pool.clone(),
            controller.clone(),
            download_manager.clone(),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            adw::ApplicationWindow::builder().build(),
        );

        assert_eq!(screen.hooks.default_speed_row.selected(), 1, "1.0× is the second speed preset");
        assert_eq!(screen.hooks.skip_back_row.selected(), 2, "15 seconds is the third skip choice");
        assert_eq!(screen.hooks.skip_forward_row.selected(), 4, "30 seconds is the fifth skip choice");
        assert!(screen.hooks.wifi_only_switch.state(), "Wi-Fi-only downloads defaults to on");
        assert_eq!(screen.hooks.theme_row.selected(), 0, "theme defaults to System");

        // `set_selected` is the property change the combo handlers listen to (a real user choice
        // emits exactly that); the switch goes through the same `state-set` emission a click
        // delivers.
        screen.hooks.default_speed_row.set_selected(3); // 1.5×
        screen.hooks.skip_back_row.set_selected(0); // 5 sec
        screen.hooks.skip_forward_row.set_selected(6); // 60 sec
        let _: bool = screen.hooks.wifi_only_switch.emit_by_name("state-set", &[&false]);
        screen.hooks.theme_row.set_selected(2); // Dark

        assert_eq!(controller.default_speed(), 1.5, "combo activation must reach the live controller");
        assert_eq!(controller.skip_intervals(), (5.0, 60.0), "skip choices must reach the live controller");
        assert!(!download_manager.wifi_only(), "the Wi-Fi switch must reach the download manager");
        assert_eq!(
            adw::StyleManager::default().color_scheme(),
            adw::ColorScheme::ForceDark,
            "the theme row must apply immediately"
        );

        // The saves are spawned futures on the GLib main context — pump until they land, same as
        // the headphone-switch scenario above.
        pump_until(|| false, Duration::from_millis(300));

        let (saved, saved_theme) = runtime.block_on(async {
            let settings = abs_core::settings::load_playback_settings(&pool).await.unwrap();
            let theme = abs_core::settings::load_theme(&pool).await.unwrap();
            (settings, theme)
        });
        assert_eq!(saved.default_speed, 1.5, "the default speed must persist");
        assert_eq!(saved.skip_back_seconds, 5, "skip back must persist");
        assert_eq!(saved.skip_forward_seconds, 60, "skip forward must persist");
        assert!(!saved.wifi_only_downloads, "the Wi-Fi switch must persist");
        assert_eq!(saved_theme, abs_core::settings::Theme::Dark, "the theme must persist");

        // About: activating the row opens the app's one about window, transient to the shell's
        // window. `ActionRowExt::activate` — the row-level activation that emits `activated` —
        // not the ambiguous widget-level one.
        adw::prelude::ActionRowExt::activate(&screen.hooks.about_row);
        pump_until(
            || gtk4::Window::list_toplevels().iter().any(|w| w.is::<adw::AboutWindow>()),
            Duration::from_secs(2),
        );
    }
}
