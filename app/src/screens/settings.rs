//! The Settings destination — per `docs/design/ui-spec.md`'s "Settings" section, an
//! `AdwPreferencesPage` under the tab bar. The **Account** group (the active server/account;
//! tapping it pushes the active server's Connection page) and the **Servers** group (one row
//! per configured server with a Switch/Sign Out/Remove menu, plus Add Server — which reuses
//! the Welcome flow, the only way a second server can ever enter the database) are real, as
//! are the **Playback** group (headphone switches, default speed, skip intervals, Wi-Fi-only
//! downloads), the **Appearance** group (Theme) and the **About** row. Still to come:
//! Playback's sleep-timer-default row — it arrives with the sleep-timer popover feature, since
//! every row here is live wiring rather than decoration.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::playback::SPEED_PRESETS;
use abs_core::settings::{PlaybackSettings, Theme};
use abs_storage::models::{Account, Server};

use crate::widgets::combo_row;
use abs_storage::AppPaths;

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
    pub account_row: adw::ActionRow,
    pub server_rows: Vec<ServerRowHooks>,
    pub add_server_row: adw::ActionRow,
}

#[cfg(test)]
pub struct ServerRowHooks {
    pub row: adw::ActionRow,
    pub switch_item: gtk4::Button,
    pub sign_out_item: gtk4::Button,
    pub remove_item: gtk4::Button,
}

/// A settings page is wired to everything it can edit — the argument count reflects that
/// surface, not a missing params-struct refactor; the allow documents that judgment call.
#[allow(clippy::too_many_arguments)]
pub fn build(
    pool: SqlitePool,
    controller: PlayerController,
    download_manager: DownloadManager,
    playback_settings: PlaybackSettings,
    theme: Theme,
    paths: AppPaths,
    servers_with_accounts: Vec<(Server, Vec<Account>)>,
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

    // --- Account: the one session the shell was built for. The DB enforces at most one active
    // account across all servers (`accounts_one_active_idx`), and the signed-in shell exists by
    // that grace — so this row is a fact, not a picker; switching happens per server below.
    // (The mockup's "Switch or manage servers" row is deliberately absent: both groups scroll on
    // one page, so the Servers list *is* that surface — no second row can add anything.) ---
    // The post-mutation shell handoff every session-changing action in these groups ends with —
    // rebuilt from the database, or the Welcome screen when no active account is left.
    let on_session_changed: Rc<dyn Fn()> = {
        let pool = pool.clone();
        let paths = paths.clone();
        let window = window.clone();
        std::rc::Rc::new(move || {
            crate::application::show_main_or_welcome(&window, pool.clone(), paths.clone(), playback_settings)
        })
    };

    let (active_server, active_account) = servers_with_accounts
        .iter()
        .find_map(|(server, accounts)| {
            accounts.iter().find(|account| account.is_active).map(|account| (server, account))
        })
        .expect("the signed-in shell requires an active account");

    let account_group = adw::PreferencesGroup::new();
    account_group.set_title("Account");
    let account_row = adw::ActionRow::builder()
        .title(&active_account.username)
        .subtitle(format!("{} · active", host_of(&active_server.url)))
        .build();
    account_row.add_suffix(&gtk4::Image::from_icon_name("go-next-symbolic"));
    account_row.set_activatable(true);
    account_row.connect_activated({
        let pool = pool.clone();
        let window = window.clone();
        let server = active_server.clone();
        let account = active_account.clone();
        let on_session_changed = on_session_changed.clone();
        move |_| push_connection(&pool, &server, Some(account.clone()), &window, &on_session_changed)
    });
    account_group.add(&account_row);
    page.add(&account_group);

    // --- Servers: one row per configured server — host as title, its logged-in username (with
    // an "active" marker on the active server's row) as subtitle — each with a trailing ⋯ menu.
    // The menu button is its own ≥44px hit target, spaced from the row body's tap target (the
    // spec's touch-target note). This is where sign-out actually lives — scoped to one
    // server/account at a time, since a global "Sign Out" would be ambiguous with multiple
    // servers supported. ---
    let servers_group = adw::PreferencesGroup::new();
    servers_group.set_title("Servers");

    #[cfg(test)]
    let mut server_rows_hooks = Vec::new();
    for (server, accounts) in &servers_with_accounts {
        let is_active_server = accounts.iter().any(|account| account.is_active);
        let subtitle = match accounts.first() {
            Some(account) if is_active_server => format!("{} · active", account.username),
            Some(account) => account.username.clone(),
            None => "No signed-in account".to_string(),
        };
        let row = adw::ActionRow::builder().title(host_of(&server.url)).subtitle(&subtitle).build();

        let menu_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();

        // "Switch to this server" flips the DB's active account to this server's and rebuilds
        // the shell around it — pointless when this server's account is already the active one,
        // and impossible when the server has none.
        let switch_item = gtk4::Button::builder()
            .label("Switch to this server")
            .css_classes(["flat"])
            .height_request(44)
            .build();
        switch_item.set_sensitive(!is_active_server && !accounts.is_empty());
        menu_box.append(&switch_item);

        let sign_out_item = gtk4::Button::builder()
            .label("Sign Out")
            .css_classes(["flat"])
            .height_request(44)
            .build();
        sign_out_item.set_sensitive(!accounts.is_empty());
        menu_box.append(&sign_out_item);

        let remove_item = gtk4::Button::builder()
            .label("Remove Server")
            .css_classes(["flat", "destructive-action"])
            .height_request(44)
            .build();
        menu_box.append(&remove_item);

        let menu_button = gtk4::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .css_classes(["flat"])
            .width_request(44)
            .height_request(44)
            .valign(gtk4::Align::Center)
            .build();
        menu_button.set_popover(Some(&gtk4::Popover::builder().child(&menu_box).build()));
        row.add_suffix(&menu_button);

        // Tapping the row's body (not the ⋯ menu) pushes the server's Connection page.
        row.set_activatable(true);
        row.connect_activated({
            let pool = pool.clone();
            let window = window.clone();
            let server = server.clone();
            let account = accounts.first().cloned();
            let on_session_changed = on_session_changed.clone();
            move |_| push_connection(&pool, &server, account.clone(), &window, &on_session_changed)
        });

        servers_group.add(&row);

        if let Some(account) = accounts.first() {
            switch_item.connect_clicked({
                let pool = pool.clone();
                let paths = paths.clone();
                let window = window.clone();
                let account_id = account.id.clone();
                move |_| {
                    let pool = pool.clone();
                    let paths = paths.clone();
                    let window = window.clone();
                    let account_id = account_id.clone();
                    glib::spawn_future_local(async move {
                        if let Err(err) = abs_core::accounts::switch_active_account(&pool, &account_id).await {
                            tracing::warn!(%err, "couldn't switch the active account");
                            return;
                        }
                        crate::application::show_main_or_welcome(&window, pool, paths, playback_settings);
                    });
                }
            });

            // Sign-out removes the account row — its local progress and bookmarks cascade away
            // with it (per-account data, keyed by FK). The server and its cached
            // libraries/items/downloads survive. Destructive enough to confirm first.
            sign_out_item.connect_clicked({
                let pool = pool.clone();
                let paths = paths.clone();
                let window = window.clone();
                let account_id = account.id.clone();
                let username = account.username.clone();
                move |_| {
                    let dialog_window = window.clone();
                    let pool = pool.clone();
                    let paths = paths.clone();
                    let window = window.clone();
                    let account_id = account_id.clone();
                    confirm(
                        &dialog_window,
                        &format!("Sign out of {username}?"),
                        &format!(
                            "{username}'s listening progress and bookmarks stored on this device will be \
                             removed — everything on the server stays where it is."
                        ),
                        "Sign Out",
                        Rc::new(move || {
                            let pool = pool.clone();
                            let paths = paths.clone();
                            let window = window.clone();
                            let account_id = account_id.clone();
                            glib::spawn_future_local(async move {
                                if let Err(err) = abs_core::accounts::sign_out(&pool, &account_id).await {
                                    tracing::warn!(%err, "couldn't sign out");
                                    return;
                                }
                                crate::application::show_main_or_welcome(&window, pool, paths, playback_settings);
                            });
                        }),
                    );
                }
            });
        }

        // Removing the server cascades every account/library/item/download record for it
        // (abs_core::accounts::remove_server) and — unlike sign-out — also purges the server's
        // on-disk cover/download files, the same cleanup a server switch via relogin performs.
        remove_item.connect_clicked({
            let pool = pool.clone();
            let paths = paths.clone();
            let window = window.clone();
            let server_id = server.id.clone();
            let server_host = host_of(&server.url).to_string();
            move |_| {
                let dialog_window = window.clone();
                let pool = pool.clone();
                let paths = paths.clone();
                let window = window.clone();
                let server_id = server_id.clone();
                confirm(
                    &dialog_window,
                    &format!("Remove {server_host}?"),
                    "Every account, cached library, item and downloaded file for this server will be \
                     removed from this device. Everything on the server itself stays untouched.",
                    "Remove Server",
                    Rc::new(move || {
                        let pool = pool.clone();
                        let paths = paths.clone();
                        let window = window.clone();
                        let server_id = server_id.clone();
                        glib::spawn_future_local(async move {
                            if let Err(err) = abs_core::accounts::remove_server(&pool, &server_id).await {
                                tracing::warn!(%err, "couldn't remove the server");
                                return;
                            }
                            if let Err(err) = paths.purge_server_data(&server_id).await {
                                tracing::warn!(%err, server_id = %server_id, "couldn't purge the removed server's on-disk files; they are orphaned but harmless");
                            }
                            crate::application::show_main_or_welcome(&window, pool, paths, playback_settings);
                        });
                    }),
                );
            }
        });

        #[cfg(test)]
        server_rows_hooks.push(ServerRowHooks {
            row: row.clone(),
            switch_item,
            sign_out_item,
            remove_item,
        });
    }

    // "Add Server" — the only way a second server can ever enter the database: the Welcome flow
    // it reuses is otherwise only reachable when no account is active at all.
    let add_server_row = adw::ActionRow::builder().title("Add Server").build();
    add_server_row.add_prefix(&gtk4::Image::from_icon_name("list-add-symbolic"));
    add_server_row.set_activatable(true);
    add_server_row.connect_activated({
        let pool = pool.clone();
        let paths = paths.clone();
        let window = window.clone();
        move |_| crate::application::show_add_server(&window, pool.clone(), paths.clone(), playback_settings)
    });
    servers_group.add(&add_server_row);
    page.add(&servers_group);

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
            account_row,
            server_rows: server_rows_hooks,
            add_server_row,
        },
    }
}

/// Pushes a server's Connection page by swapping the window's content — the same mechanism the
/// full player uses (`AdwNavigationView` is out of reach at this crate's libadwaita `v1_2`
/// ceiling). The page's back button restores the shell widget captured here, so no shell state
/// is lost.
fn push_connection(
    pool: &SqlitePool,
    server: &Server,
    account: Option<Account>,
    window: &adw::ApplicationWindow,
    on_session_changed: &Rc<dyn Fn()>,
) {
    let Some(shell_root) = window.content() else { return };
    let screen = crate::screens::connection::build(
        pool.clone(),
        server.clone(),
        account,
        window,
        &shell_root,
        on_session_changed.clone(),
    );
    crate::widgets::swap_content(window, &screen.root);
}

/// The `host[:port]` part of a server URL — what Account/Servers rows show instead of the full
/// scheme-and-path form (the URL in full stays on the server's Connection page).
pub(crate) fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

/// A modal Ok/Cancel confirmation for a destructive session change — the same
/// `GtkMessageDialog` pattern the Welcome screen's replace-data confirmation uses. Cancel just
/// destroys the dialog; the confirmed action runs only on the affirmative response.
pub(crate) fn confirm(
    window: &adw::ApplicationWindow,
    heading: &str,
    body: &str,
    confirm_label: &str,
    on_confirm: Rc<dyn Fn()>,
) {
    let dialog = gtk4::MessageDialog::builder()
        .message_type(gtk4::MessageType::Warning)
        .text(heading)
        .secondary_text(body)
        .modal(true)
        .transient_for(window)
        .build();
    dialog.add_button("Cancel", gtk4::ResponseType::Cancel);
    let confirm_button = dialog.add_button(confirm_label, gtk4::ResponseType::Ok);
    confirm_button.add_css_class("destructive-action");
    dialog.connect_response(move |dialog, response| {
        dialog.destroy();
        if response == gtk4::ResponseType::Ok {
            on_confirm();
        }
    });
    dialog.present();
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

    /// One active server/account — the minimum the signed-in shell (and therefore the Account
    /// group) requires. Returns the data `build` threads into the screen, in its shape.
    fn seed_active_session(
        runtime: &tokio::runtime::Runtime,
        pool: &sqlx::SqlitePool,
        url: &str,
        username: &str,
    ) -> (abs_storage::models::Server, Vec<abs_storage::models::Account>) {
        runtime.block_on(async {
            let server_id = abs_storage::repo::servers::add(pool, url).await.unwrap();
            let account_id = abs_storage::repo::accounts::add(pool, &server_id, username, "token", None).await.unwrap();
            abs_storage::repo::accounts::set_active(pool, &account_id).await.unwrap();
            let server = abs_storage::repo::servers::get(pool, &server_id).await.unwrap();
            let account = abs_storage::repo::accounts::get(pool, &account_id).await.unwrap();
            (server, vec![account])
        })
    }

    fn find_message_dialog() -> Option<gtk4::MessageDialog> {
        gtk4::Window::list_toplevels()
            .into_iter()
            .find_map(|window| window.downcast::<gtk4::MessageDialog>().ok())
    }

    /// Depth-first search for a button by label — how the Add-Server scenario reaches the
    /// Welcome screen's Cancel button, which belongs to a screen built inside
    /// `application::show_add_server` (no hooks cross that boundary).
    fn find_button_with_label(widget: &gtk4::Widget, label: &str) -> Option<gtk4::Button> {
        if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
            if button.label().is_some_and(|text| text == label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_button_with_label(&current, label) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let screen = build(
            pool.clone(),
            controller.clone(),
            test_download_manager(pool.clone()),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            crate::test_support::test_paths(),
            servers,
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
        let servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let download_manager = test_download_manager(pool.clone());
        let screen = build(
            pool.clone(),
            controller.clone(),
            download_manager.clone(),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            crate::test_support::test_paths(),
            servers,
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

    /// The Account and Servers groups mirror the database: the active account's row (username,
    /// host · active), one row per server with its username and active marker, and per-server
    /// menu items whose sensitivity follows who's active. Includes the one action that doesn't
    /// destroy data — switching the active server — end to end.
    pub(crate) fn run_account_and_servers_rows_reflect_the_database(runtime: &tokio::runtime::Runtime) {
        // Two pools on one file: the rebuilt shell's background tasks (home/library syncs against
        // unreachable servers) hold the main pool's connections for a long time, so post-rebuild
        // asserts go through their own idle pool instead of contending with them.
        let (pool, assert_pool) = runtime.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let db_path = tmp.path().join("db.sqlite3");
            std::mem::forget(tmp);
            let pool = abs_storage::connect_and_migrate(&db_path).await.unwrap();
            let assert_pool = abs_storage::connect_and_migrate(&db_path).await.unwrap();
            (pool, assert_pool)
        });
        let mut servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
        servers.push(seed_active_session_but_inactive(runtime, &pool, "http://127.0.0.1:2", "bob"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let app_window = adw::ApplicationWindow::builder().build();
        let screen = build(
            pool.clone(),
            controller,
            test_download_manager(pool.clone()),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            crate::test_support::test_paths(),
            servers,
            app_window.clone(),
        );
        let hooks = &screen.hooks;

        assert_eq!(hooks.account_row.title(), "jane");
        assert_eq!(hooks.account_row.subtitle().as_deref(), Some("127.0.0.1:1 · active"));

        assert_eq!(hooks.server_rows.len(), 2, "one row per configured server");
        assert_eq!(hooks.server_rows[0].row.title(), "127.0.0.1:1");
        assert_eq!(hooks.server_rows[0].row.subtitle().as_deref(), Some("jane · active"));
        assert_eq!(hooks.server_rows[1].row.title(), "127.0.0.1:2");
        assert_eq!(hooks.server_rows[1].row.subtitle().as_deref(), Some("bob"));

        assert!(
            !hooks.server_rows[0].switch_item.is_sensitive(),
            "the active server can't be switched to — it's already active"
        );
        assert!(hooks.server_rows[0].sign_out_item.is_sensitive() && hooks.server_rows[0].remove_item.is_sensitive());

        // Tapping the Account row's body pushes the active server's Connection page — a window
        // content swap, not a rebuild (the page's back button is covered by the connection
        // scenario).
        let old_root = screen.root.clone();
        app_window.set_content(Some(&old_root));
        adw::prelude::ActionRowExt::activate(&hooks.account_row);
        pump_until(
            {
                let old_root = old_root.clone();
                let app_window = app_window.clone();
                move || app_window.content().as_ref() != Some(&old_root)
            },
            Duration::from_secs(5),
        );

        // Switching to the second server: the DB's active account flips, and the shell the
        // settings screen lives in is rebuilt around the new session (here observed as the
        // window's content being swapped off the screen this build returned).
        let old_root = screen.root.clone();
        app_window.set_content(Some(&old_root));
        hooks.server_rows[1].switch_item.emit_clicked();
        pump_until(
            {
                let old_root = old_root.clone();
                let app_window = app_window.clone();
                move || app_window.content().as_ref() != Some(&old_root)
            },
            Duration::from_secs(10),
        );
        let active = runtime.block_on(abs_storage::repo::accounts::get_active(&assert_pool)).unwrap().unwrap();
        assert_eq!(active.username, "bob", "switching must make the second server's account active");
    }

    /// The two destructive menu actions, end to end: confirmed via the same MessageDialog
    /// pattern the Welcome screen's replacement confirmation uses, and both hand control back
    /// to `application::show_main_or_welcome` — signing out the only account lands on the
    /// first-run Welcome screen, and removing the server also purges its on-disk files.
    pub(crate) fn run_servers_menu_actions_rebuild_the_shell(runtime: &tokio::runtime::Runtime) {
        // --- Sign out: confirmed, then the account (and its local progress) is gone. ---
        {
            let pool = runtime.block_on(crate::test_support::pool());
            let servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
            let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
            let app_window = adw::ApplicationWindow::builder().build();
            let screen = build(
                pool.clone(),
                controller,
                test_download_manager(pool.clone()),
                abs_core::settings::PlaybackSettings::default(),
                abs_core::settings::Theme::default(),
                crate::test_support::test_paths(),
                servers,
                app_window.clone(),
            );
            let old_root = screen.root.clone();
            app_window.set_content(Some(&old_root));

            screen.hooks.server_rows[0].sign_out_item.emit_clicked();
            pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
            find_message_dialog().unwrap().response(gtk4::ResponseType::Cancel);
            pump_until(|| find_message_dialog().is_none(), Duration::from_secs(5));
            assert!(
                runtime.block_on(abs_storage::repo::accounts::get_active(&pool)).unwrap().is_some(),
                "a cancelled sign-out must leave the session intact"
            );

            screen.hooks.server_rows[0].sign_out_item.emit_clicked();
            pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
            find_message_dialog().unwrap().response(gtk4::ResponseType::Ok);
            pump_until(
                {
                    let old_root = old_root.clone();
                    let app_window = app_window.clone();
                    move || app_window.content().as_ref() != Some(&old_root)
                },
                Duration::from_secs(10),
            );
            assert!(
                runtime.block_on(abs_storage::repo::accounts::get_active(&pool)).unwrap().is_none(),
                "a confirmed sign-out must remove the account — with no account left, the shell hands over to Welcome"
            );
        }

        // --- Remove server: confirmed, then rows cascade and the on-disk cache is purged. ---
        {
            let pool = runtime.block_on(crate::test_support::pool());
            let servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
            let paths = crate::test_support::test_paths();
            let cover = paths.cover_cache_path(&servers[0].0.id, "item-1", "jpg");
            std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
            std::fs::write(&cover, b"bytes").unwrap();

            let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
            let app_window = adw::ApplicationWindow::builder().build();
            let screen = build(
                pool.clone(),
                controller,
                test_download_manager(pool.clone()),
                abs_core::settings::PlaybackSettings::default(),
                abs_core::settings::Theme::default(),
                paths,
                servers,
                app_window.clone(),
            );
            let old_root = screen.root.clone();
            app_window.set_content(Some(&old_root));

            screen.hooks.server_rows[0].remove_item.emit_clicked();
            pump_until(|| find_message_dialog().is_some(), Duration::from_secs(5));
            find_message_dialog().unwrap().response(gtk4::ResponseType::Ok);
            pump_until(
                {
                    let old_root = old_root.clone();
                    let app_window = app_window.clone();
                    move || app_window.content().as_ref() != Some(&old_root)
                },
                Duration::from_secs(10),
            );
            assert!(runtime.block_on(abs_storage::repo::servers::list(&pool)).unwrap().is_empty(), "the server row must be gone");
            assert!(!cover.exists(), "the server's on-disk cover cache must be purged, not orphaned");
        }
    }

    /// "Add Server" reuses the Welcome flow over the signed-in shell — and, unlike the first
    /// run, it must offer a way back: Cancel restores the shell exactly as it was (the captured
    /// root widget, not a rebuild).
    pub(crate) fn run_add_server_row_opens_welcome_and_cancels_back(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let servers = vec![seed_active_session(runtime, &pool, "http://127.0.0.1:1", "jane")];
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        let app_window = adw::ApplicationWindow::builder().build();
        let screen = build(
            pool.clone(),
            controller,
            test_download_manager(pool.clone()),
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            crate::test_support::test_paths(),
            servers,
            app_window.clone(),
        );
        let old_root = screen.root.clone();
        app_window.set_content(Some(&old_root));

        adw::prelude::ActionRowExt::activate(&screen.hooks.add_server_row);
        pump_until(
            {
                let old_root = old_root.clone();
                let app_window = app_window.clone();
                move || app_window.content().as_ref() != Some(&old_root)
            },
            Duration::from_secs(5),
        );
        let content = app_window.content().expect("the Add-Server flow swaps the window's content");
        // `is_visible` walks up to the toplevel — an unpresented window would make everything
        // report invisible, so map it first (same as the shell scenario's Ctrl+F focus check).
        app_window.present();
        pump_until(|| app_window.is_mapped(), Duration::from_secs(5));
        let cancel = find_button_with_label(&content, "Cancel")
            .expect("an Add-Server flow started from Settings must be cancelable — the shell is still behind it");
        assert!(cancel.is_visible());

        cancel.emit_clicked();
        pump_until(
            {
                let old_root = old_root.clone();
                let app_window = app_window.clone();
                move || app_window.content().as_ref() == Some(&old_root)
            },
            Duration::from_secs(10),
        );
    }

    /// `seed_active_session` for a *second* server: its account is created but never activated, the
    /// shape the Servers group must render for a server you're not currently using.
    fn seed_active_session_but_inactive(
        runtime: &tokio::runtime::Runtime,
        pool: &sqlx::SqlitePool,
        url: &str,
        username: &str,
    ) -> (abs_storage::models::Server, Vec<abs_storage::models::Account>) {
        runtime.block_on(async {
            let server_id = abs_storage::repo::servers::add(pool, url).await.unwrap();
            let account_id = abs_storage::repo::accounts::add(pool, &server_id, username, "token", None).await.unwrap();
            let server = abs_storage::repo::servers::get(pool, &server_id).await.unwrap();
            let account = abs_storage::repo::accounts::get(pool, &account_id).await.unwrap();
            (server, vec![account])
        })
    }
}
