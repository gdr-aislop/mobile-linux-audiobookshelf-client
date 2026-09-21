//! The post-login shell: a 4-destination bottom tab bar (Home / Library / Downloads / Settings),
//! per `docs/design/ui-spec.md` §2's phone-width navigation model. Built from `AdwViewStack` +
//! `AdwViewSwitcherBar` — both available at this crate's libadwaita `v1_2` ceiling (confirmed by
//! reading `libadwaita-0.7.2/src/auto/mod.rs`'s cfg gates: neither is behind a version feature
//! gate, unlike `AdwToolbarView`/`AdwBreakpoint`/`AdwNavigationView`/`AdwNavigationSplitView`,
//! which are all `v1_4` and out of reach here) — so, unlike the mini-player bar or the Welcome
//! screen's auth-mode toggle, this needs no hand-rolled compatibility widget.
//!
//! Only Home has real content so far; Library/Downloads/Settings are `AdwStatusPage` stubs, each
//! still a real page in the stack so wiring in an actual screen later is a one-line swap. No
//! wide-screen sidebar layout (`AdwNavigationSplitView`/`AdwBreakpoint`, both v1.4+) — phone-width
//! single-pane only, left as a documented follow-up.

use std::rc::Rc;

use abs_player::call_watch::CallWatcher;
use abs_player::route_watch::RouteWatcher;
use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::settings::{PlaybackSettings, Theme};
use abs_storage::models::{Account, Server};
use abs_storage::AppPaths;

use crate::player::{self, PlayRequest};
use crate::screens;

pub struct MainWindow {
    pub root: gtk4::Widget,
    /// Kept alive for the app's whole lifetime — dropping it unsubscribes from ModemManager's
    /// D-Bus signals. `None` when no system bus (or no ModemManager on it) was reachable; call
    /// interruption is simply unavailable in that case, never a fatal error.
    _call_watcher: Option<abs_player::call_watch::ModemManagerCallWatcher>,
    /// Same lifetime contract as `_call_watcher`: retained forever, `None` when no audio server
    /// (PulseAudio/PipeWire) was reachable — headphone unplug/replug reaction is then simply
    /// unavailable, never a fatal error.
    _route_watcher: Option<abs_player::route_watch::PulseRouteWatcher>,
    /// Kept alive for the app's whole lifetime, same reasoning as `_call_watcher` — dropping it
    /// would lose every in-flight track's cancel flag and listener. Every screen that needs it
    /// (Downloads, Settings, Item Detail, Player) is handed its own clone at build time; this
    /// field itself is never read back (hence the allow), only retained.
    #[allow(dead_code)]
    pub download_manager: crate::downloads::DownloadManager,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub stack: adw::ViewStack,
    pub switcher_bar: adw::ViewSwitcherBar,
    pub mini_bar: crate::player::MiniPlayerHooks,
    /// The shell's keyboard actions (ui-spec §6), so tests can activate them directly — the
    /// accel-to-action mapping itself is GTK-level and needs real key events to exercise.
    pub switch_tab: gtk4::gio::SimpleAction,
    pub play_pause: gtk4::gio::SimpleAction,
    pub bookmark: gtk4::gio::SimpleAction,
    pub open_library_search: gtk4::gio::SimpleAction,
    /// The Library tab's search entry, the focus target of `open-library-search`.
    pub library_search: gtk4::SearchEntry,
    /// Settings → Playback's headphone-behavior switches, so tests can assert initial state and
    /// activation (the row-tap path) without reaching into the screen's internals.
    pub pause_on_unplug_switch: gtk4::Switch,
    pub resume_on_replug_switch: gtk4::Switch,
}

#[cfg(test)]
impl MainWindow {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

/// A signed-in shell's construction from everything it needs. The argument count is the shell's
/// genuine composition — pool, paths, the session it fronts, the two long-lived managers, the
/// screen data and the window itself — not an accident to refactor into a params struct; the
/// allow documents that judgment call.
#[allow(clippy::too_many_arguments)]
pub fn build(
    pool: SqlitePool,
    paths: AppPaths,
    server: Server,
    account: Account,
    session: abs_core::auth::Session,
    playback_settings: PlaybackSettings,
    theme: Theme,
    servers_with_accounts: Vec<(Server, Vec<Account>)>,
    window: adw::ApplicationWindow,
) -> MainWindow {
    let mini_bar = player::build_mini_bar(pool.clone(), paths.clone(), player::real_backend());

    // MPRIS registration is best-effort — no session bus (a bare console, a locked-down sandbox)
    // must never be fatal to playback, so a failure here is just a warning. On success, the
    // bridge becomes a permanent snapshot listener (via the foundation refactor's `add_listener`)
    // so the lock-screen/Shell media card stays current for the app's whole lifetime.
    let mpris_bridge: Rc<dyn abs_player::mpris::MprisCommands> = Rc::new(player::MprisBridge::new(mini_bar.controller.clone()));
    match abs_player::mpris::register("Audiobookshelf", mpris_bridge) {
        Ok(mpris_handle) => {
            mini_bar.controller.add_listener(move |snapshot| mpris_handle.update(player::mpris_state_from_snapshot(snapshot)));
        }
        Err(err) => tracing::warn!(%err, "couldn't register MPRIS media player; system media integration will be unavailable"),
    }

    // Phone-call interruption is best-effort in the same way: no system bus, or no ModemManager
    // on it, must never be fatal — it just means this feature is unavailable. There is
    // deliberately no "call ended" handling anywhere (see `call_watch`'s module docs): a call
    // going active only ever pauses, never auto-resumes. The watcher itself is retained on
    // `MainWindow` (see its field doc) — dropping it would unsubscribe immediately.
    let call_watcher = match abs_player::call_watch::ModemManagerCallWatcher::new() {
        Ok(mut watcher) => {
            let controller = mini_bar.controller.clone();
            watcher.start(Box::new(move || controller.pause()));
            Some(watcher)
        }
        Err(err) => {
            tracing::warn!(%err, "couldn't watch ModemManager for call interruptions; this feature will be unavailable");
            None
        }
    };

    // Headphone unplug/replug (see `route_watch`'s module docs) — best-effort in the same way:
    // no audio server to talk to means the feature is unavailable, never fatal. The behavior
    // knobs come from Settings → Playback and are applied before the first event can arrive.
    // The callback is one line by design: all the semantics (only unplug pauses; a replug only
    // lifts an unplug pause, never a manual one or a call) live in `handle_route_event`, where
    // the tests can reach them.
    mini_bar.controller.set_headphone_behavior(
        playback_settings.pause_on_headphone_unplug,
        playback_settings.resume_on_headphone_replug,
    );
    // Same story for the playback config: the start-of-session speed and skip intervals live on
    // the controller, and the consumers read them at call time — a Settings edit reaches even
    // MPRIS's next/previous without rebuilding any screen.
    mini_bar.controller.set_playback_config(
        playback_settings.default_speed,
        playback_settings.skip_back_seconds as f64,
        playback_settings.skip_forward_seconds as f64,
    );
    let route_watcher = match abs_player::route_watch::PulseRouteWatcher::new() {
        Ok(mut watcher) => {
            let controller = mini_bar.controller.clone();
            watcher.start(Box::new(move |event| controller.handle_route_event(event)));
            Some(watcher)
        }
        Err(err) => {
            tracing::warn!(%err, "couldn't watch the audio server for headphone changes; unplug-pause will be unavailable");
            None
        }
    };

    // Wi-Fi-only detection is best-effort in the same way as MPRIS/call-watching above: no system
    // bus (or no NetworkManager on it) must never be fatal — it just means metered-connection
    // detection is unavailable, and `UnknownNetworkMonitor` makes that read as "undeterminable"
    // rather than silently guessing "not metered".
    let network_monitor: Box<dyn abs_player::network_watch::NetworkMonitor> = match abs_player::network_watch::NetworkManagerMonitor::new() {
        Ok(monitor) => Box::new(monitor),
        Err(err) => {
            tracing::warn!(%err, "couldn't watch NetworkManager for metered-connection detection; Wi-Fi-only downloads will be unavailable");
            Box::new(abs_player::network_watch::UnknownNetworkMonitor)
        }
    };
    let download_manager = crate::downloads::DownloadManager::new(pool.clone(), paths.clone(), network_monitor, playback_settings.wifi_only_downloads);
    // Shared between Home and Library only (per docs/design/ui-spec.md, "not a per-screen
    // setting") — see `crate::offline_mode::OfflineModeState`'s doc for why this must be a single
    // shared instance rather than each screen loading its own copy.
    let offline_mode = crate::offline_mode::OfflineModeState::new(pool.clone());

    let stack = adw::ViewStack::new();

    // Starts real playback (`PlayerController::start`) for a request, optionally seeking to a
    // chapter once it's ready — the one place in the shell that actually calls into
    // `abs-player`/`abs-core::streaming` on a card's behalf. Used below both as Item Detail's own
    // `on_play` (a chapter tap or Play/Resume) — Item Detail itself never touches the controller
    // directly, same "screens report intent, the shell acts on it" boundary `home.rs`/`library.rs`
    // already draw for `on_open`.
    let start_playback = {
        let controller = mini_bar.controller.clone();
        let session = session.clone();
        move |request: PlayRequest, start_chapter: Option<usize>| {
            // The default speed is read at call time, not captured — a "Default speed" change in
            // Settings applies to the next playback without rebuilding the shell.
            controller.start(session.clone(), request, controller.default_speed());
            if let Some(index) = start_chapter {
                // Chapters aren't known until `start()`'s async resolve lands, so seeking to the
                // tapped chapter polls for readiness the same bounded way (50 * 100ms)
                // `PlayerController::start` itself already waits for the pipeline to become
                // seekable before applying its own resume seek.
                let controller = controller.clone();
                glib::spawn_future_local(async move {
                    for _ in 0..50 {
                        if let Some(chapter) = controller.chapters().get(index) {
                            controller.seek_to_seconds(chapter.start_seconds);
                            return;
                        }
                        glib::timeout_future(std::time::Duration::from_millis(100)).await;
                    }
                });
            }
        }
    };

    // "Log in again" (Home/Library's authorization-failure states) hands control back to the
    // shell: the window's content is swapped for the login screen, pre-filled with this session's
    // URL and username so only the password is left to type. The seed describes the session this
    // shell was built for — exactly what a re-login compares against.
    let on_relogin = {
        let window = window.clone();
        let pool = pool.clone();
        let paths = paths.clone();
        // Cloned into locals first: a `move` closure would otherwise capture the `server`/
        // `account` fields it touches by value (editions' disjoint field capture), pulling the
        // rug out from under the `server.clone()`/`account.clone()` calls further down.
        let seed_server_id = server.id.clone();
        let seed_server_url = server.url.clone();
        let seed_account_id = account.id.clone();
        let seed_username = account.username.clone();
        move || {
            let seed = abs_core::accounts::ReloginSeed {
                server_id: seed_server_id.clone(),
                account_id: seed_account_id.clone(),
                url: seed_server_url.clone(),
                username: seed_username.clone(),
            };
            crate::application::show_welcome(&window, pool.clone(), paths.clone(), playback_settings, Some(seed));
        }
    };

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // Tapping a cover card opens Item Detail (ui-spec's real "tap -> Item detail -> Play" flow) by
    // swapping the window's content in — the same content-swap mechanism the mini-bar's
    // tap-to-expand already uses for Player (there's no `AdwNavigationView`/`AdwDialog` at this
    // crate's libadwaita ceiling). Constructed on demand, per tap, exactly mirroring how the
    // mini-bar gesture below builds a fresh `PlayerScreen` on every open rather than keeping one
    // around; `on_back` restores `root` (this shell) the same way Player's `on_collapse` does.
    let on_open = {
        let window = window.clone();
        let root = root.clone();
        let pool = pool.clone();
        let server = server.clone();
        let account = account.clone();
        let session = session.clone();
        let download_manager = download_manager.clone();
        let start_playback = Rc::new(start_playback);
        move |request: PlayRequest| {
            let on_play = {
                let start_playback = start_playback.clone();
                let request = request.clone();
                move |item_id: String, start_chapter: Option<usize>| {
                    debug_assert_eq!(item_id, request.item_id, "Item Detail should only ever report its own item id back");
                    start_playback(request.clone(), start_chapter);
                }
            };
            let on_back = {
                let window = window.clone();
                let root = root.clone();
                move || window.set_content(Some(&root))
            };
            let item_detail_screen = screens::item_detail::build(
                pool.clone(),
                server.clone(),
                account.clone(),
                session.clone(),
                download_manager.clone(),
                request.item_id.clone(),
                on_play,
                on_back,
            );
            window.set_content(Some(&item_detail_screen.root));
        }
    };

    // Library is built before Home only so the tap-through closure below can capture the
    // already-built screen; the `add_titled_with_icon` calls (not build order) fix the
    // switcher's tab order — Home stays first.
    let library_screen = screens::library::build(pool.clone(), paths.clone(), server.clone(), account.clone(), session.clone(), offline_mode.clone(), on_open.clone(), on_relogin.clone());

    // Home's shelf headings' tap-through (ui-spec Home section): switch to Library and land on
    // the shelf's view — Recently Added pre-sorted by date added; Continue Listening pre-sorted
    // by last listened and filtered to in-progress books. Session-transient both, exactly like
    // manual sort/filter changes — navigation-with-intent, not a preference.
    let on_open_shelf = {
        let stack = stack.clone();
        let library_screen = library_screen.clone();
        move |shelf| {
            stack.set_visible_child_name("library");
            match shelf {
                crate::screens::home::Shelf::RecentlyAdded => library_screen.apply_view(crate::screens::library::SortKey::DateAdded, false),
                crate::screens::home::Shelf::ContinueListening => library_screen.apply_view(crate::screens::library::SortKey::LastListened, true),
            }
        }
    };

    stack.add_titled_with_icon(
        &screens::home::build(
            pool.clone(),
            paths.clone(),
            server.clone(),
            account.clone(),
            session.clone(),
            offline_mode.clone(),
            on_open.clone(),
            on_relogin,
            on_open_shelf,
        )
        .root,
        Some("home"),
        "Home",
        "go-home-symbolic",
    );
    stack.add_titled_with_icon(&library_screen.root, Some("library"), "Library", "system-file-manager-symbolic");
    let downloads_screen = screens::downloads::build(pool.clone(), paths.clone(), server, account, session, download_manager.clone());
    stack.add_titled_with_icon(&downloads_screen.root, Some("downloads"), "Downloads", "folder-download-symbolic");
    let settings_screen = screens::settings::build(
        pool.clone(),
        mini_bar.controller.clone(),
        download_manager.clone(),
        playback_settings,
        theme,
        paths,
        servers_with_accounts,
        window.clone(),
    );
    stack.add_titled_with_icon(&settings_screen.root, Some("settings"), "Settings", "emblem-system-symbolic");

    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).reveal(true).build();

    root.append(&stack);
    root.append(&mini_bar.root);
    root.append(&switcher_bar);

    // Keyboard actions for the whole shell (ui-spec §6's global table; the accelerators
    // themselves are set app-wide in `application.rs`). Transport keys no-op when nothing is
    // loaded — every `PlayerController` method already does. Single-key bindings (`space`, `b`)
    // rely on GTK's focus-first shortcut resolution: while a text entry has focus, the key types
    // and the accelerator never fires, so no per-widget guards are wanted here.
    let play_pause_action = gtk4::gio::SimpleAction::new("play-pause", None);
    play_pause_action.connect_activate({
        let controller = mini_bar.controller.clone();
        move |_, _| controller.toggle_play_pause()
    });
    window.add_action(&play_pause_action);

    let bookmark_action = gtk4::gio::SimpleAction::new("bookmark", None);
    bookmark_action.connect_activate({
        let controller = mini_bar.controller.clone();
        move |_, _| controller.add_bookmark()
    });
    window.add_action(&bookmark_action);

    let switch_tab_action = gtk4::gio::SimpleAction::new("switch-tab", Some(gtk4::glib::VariantTy::STRING));
    switch_tab_action.connect_activate({
        let stack = stack.clone();
        move |_, parameter| {
            let Some(name) = parameter.and_then(|p| p.str()) else { return };
            // Unknown names are ignored rather than an error: the action exists so accelerators
            // can target it, not as a general navigation API.
            if stack.child_by_name(name).is_some() {
                stack.set_visible_child_name(name);
            }
        }
    });
    window.add_action(&switch_tab_action);

    let open_library_search_action = gtk4::gio::SimpleAction::new("open-library-search", None);
    open_library_search_action.connect_activate({
        let stack = stack.clone();
        let search_entry = library_screen.search_entry.clone();
        move |_, _| {
            stack.set_visible_child_name("library");
            search_entry.grab_focus();
        }
    });
    window.add_action(&open_library_search_action);

    // Tapping the mini bar opens the full player by swapping the window's content — there's no
    // `AdwNavigationView`/`AdwDialog` available at this crate's libadwaita ceiling (both v1.4+),
    // so this is the same content-swap mechanism `application.rs` already uses for Welcome -> main
    // window. Collapsing restores `root` (this shell), not a fresh `build()` call — no state lost.
    //
    // The player screen's keyboard actions ride along: its `SimpleActionGroup` is merged under
    // the "player" prefix for exactly as long as the screen is open, and removed on collapse, so
    // the arrow-key/speed/`c`/`t`/Escape accelerators (ui-spec §6) are inert everywhere else.
    let mini_bar_gesture = gtk4::GestureClick::new();
    mini_bar_gesture.connect_released({
        let controller = mini_bar.controller.clone();
        let window = window.clone();
        let root = root.clone();
        let pool = pool.clone();
        let download_manager = download_manager.clone();
        move |_, _, _, _| {
            let player_screen = screens::player::build(pool.clone(), controller.clone(), download_manager.clone(), {
                let window = window.clone();
                let root = root.clone();
                move || {
                    window.set_content(Some(&root));
                    window.insert_action_group("player", None::<&gtk4::gio::ActionGroup>);
                }
            });
            window.insert_action_group("player", Some(player_screen.actions.upcast_ref::<gtk4::gio::ActionGroup>()));
            window.set_content(Some(&player_screen.root));
        }
    });
    mini_bar.root.add_controller(mini_bar_gesture);

    MainWindow {
        root: root.upcast(),
        _call_watcher: call_watcher,
        _route_watcher: route_watcher,
        download_manager,
        #[cfg(test)]
        hooks: TestHooks {
            stack,
            switcher_bar,
            mini_bar: mini_bar.hooks,
            switch_tab: switch_tab_action,
            play_pause: play_pause_action,
            bookmark: bookmark_action,
            open_library_search: open_library_search_action,
            library_search: library_screen.search_entry,
            pause_on_unplug_switch: settings_screen.hooks.pause_on_unplug_switch,
            resume_on_replug_switch: settings_screen.hooks.resume_on_replug_switch,
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until};

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point. Points the Home tab's
    /// server at an address nothing listens on (`127.0.0.1:1`, immediate connection-refused) —
    /// this test only cares about the stack's shape, not sync behavior, so it must never make a
    /// real network call.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let server_id = runtime.block_on(abs_storage::repo::servers::add(&pool, "http://127.0.0.1:1")).unwrap();
        let account_id =
            runtime.block_on(abs_storage::repo::accounts::add(&pool, &server_id, "jane", "token", None)).unwrap();
        runtime.block_on(abs_storage::repo::accounts::set_active(&pool, &account_id)).unwrap();
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &account_id)).unwrap();

        let app_window = adw::ApplicationWindow::builder().build();
        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let servers_with_accounts = vec![(server.clone(), vec![account.clone()])];
        let window = build(
            pool,
            crate::test_support::test_paths(),
            server,
            account,
            session,
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            servers_with_accounts,
            app_window.clone(),
        );
        let hooks = window.test_hooks();

        for name in ["home", "library", "downloads", "settings"] {
            assert!(hooks.stack.child_by_name(name).is_some(), "missing destination: {name}");
        }
        assert!(hooks.switcher_bar.reveals(), "the tab bar should always be shown");
        assert!(!hooks.mini_bar.bar.is_visible(), "the mini bar should stay hidden until something plays");

        // The Settings tab is real now — its switches must reflect the `PlaybackSettings` the
        // shell was built with (the defaults here), so a user sees reality, not stale state.
        assert!(hooks.pause_on_unplug_switch.state(), "pause-on-unplug defaults to on");
        assert!(!hooks.resume_on_replug_switch.state(), "resume-on-replug defaults to off");

        // Keyboard actions (ui-spec §6), activated via their handles — the accel-to-action
        // mapping itself is GTK-level and can't be driven without real key events.
        hooks.switch_tab.activate(Some(&"settings".to_variant()));
        assert_eq!(hooks.stack.visible_child_name().as_deref(), Some("settings"));
        hooks.switch_tab.activate(Some(&"library".to_variant()));
        assert_eq!(hooks.stack.visible_child_name().as_deref(), Some("library"));

        // Ctrl+F's action focuses the search entry — which only takes hold on a mapped window
        // (same reason `run_chapters_sheet_lists_and_seeks` presents its window).
        app_window.present();
        pump_until(|| app_window.is_mapped(), std::time::Duration::from_secs(5));
        hooks.open_library_search.activate(None::<&gtk4::glib::Variant>);
        assert_eq!(hooks.stack.visible_child_name().as_deref(), Some("library"), "search should land on the Library tab");
        pump_until(
            || gtk4::prelude::GtkWindowExt::focus(&app_window).is_some_and(|focused| focused == *hooks.library_search.upcast_ref::<gtk4::Widget>()),
            std::time::Duration::from_secs(5),
        );

        // Transport/bookmark keys with nothing loaded are no-ops by contract (every
        // `PlayerController` method early-returns) — activating them must simply not panic.
        hooks.play_pause.activate(None::<&gtk4::glib::Variant>);
        hooks.bookmark.activate(None::<&gtk4::glib::Variant>);
    }

    /// Covers the actual risk in the call-interruption wiring — the closure `build()` registers
    /// with `ModemManagerCallWatcher` — without needing a real system bus or ModemManager (this
    /// sandbox has neither; see `abs_player::call_watch`'s own tests for the D-Bus-level
    /// coverage). `abs_player::call_watch::FakeCallWatcher` is `#[cfg(test)]`-only inside
    /// `abs-player` and so isn't visible across the crate boundary from here, but the wiring
    /// itself is just one line (`move || controller.pause()`) — reproducing and invoking that
    /// exact closure is what actually needs checking, not the D-Bus plumbing around it.
    pub(crate) fn run_call_interruption_wiring_pauses_playback(runtime: &tokio::runtime::Runtime) {
        use crate::player::tests::{account_and_server, insert_synced_item, mock_playable_item, test_backend};
        use crate::player::PlayRequest;

        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));
        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(abs_core::auth::Session::new(pool.clone(), &server, &account), PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None }, 1.0);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));

        let on_call_active: Box<dyn Fn()> = {
            let controller = controller.clone();
            Box::new(move || controller.pause())
        };
        on_call_active();

        assert!(!controller.snapshot().unwrap().is_playing, "a call becoming active should pause playback");
        controller.stop();
    }

    /// `handle_route_event`'s full behavior matrix — all the semantics that the one-line closure
    /// `build()` registers with `PulseRouteWatcher` delegates to (the watcher's own
    /// audio-server-level classification lives in `abs-player`'s tests; the FakeRouteWatcher
    /// isn't visible across the crate boundary, exactly like the call watcher's fake).
    pub(crate) fn run_headphone_route_events_follow_the_settings(runtime: &tokio::runtime::Runtime) {
        use crate::player::tests::{account_and_server, insert_synced_item, mock_playable_item, test_backend};
        use crate::player::PlayRequest;

        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 8));
        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let start_book = |controller: &crate::player::PlayerController| {
            controller.start(
                abs_core::auth::Session::new(pool.clone(), &server, &account),
                PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
                1.0,
            );
        };
        let unplug = abs_player::route_watch::RouteEvent::Unplugged;
        let replug = abs_player::route_watch::RouteEvent::Replugged;

        // Defaults (pause on, resume off): unplug pauses, replug stays paused.
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.set_headphone_behavior(true, false);
        start_book(&controller);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));
        controller.handle_route_event(unplug);
        assert!(!controller.snapshot().unwrap().is_playing, "unplugging headphones should pause playback");
        controller.handle_route_event(replug);
        assert!(!controller.snapshot().unwrap().is_playing, "with resume off (the default), a replug stays paused");
        controller.stop();

        // Resume on: a replug undoes exactly the unplug pause.
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.set_headphone_behavior(true, true);
        start_book(&controller);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));
        controller.handle_route_event(unplug);
        assert!(!controller.snapshot().unwrap().is_playing, "unplugging headphones should pause playback");
        controller.handle_route_event(replug);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));
        controller.stop();

        // A manual pause is never overridden by a replug — even with resume on, and even when
        // the unplug happened while already paused (an unplug of a paused player is not a
        // pause *caused by* the unplug).
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.set_headphone_behavior(true, true);
        start_book(&controller);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));
        // The manual pause (also the path a phone call takes) clears the "paused by unplug" mark.
        controller.pause();
        controller.handle_route_event(unplug);
        controller.handle_route_event(replug);
        assert!(!controller.snapshot().unwrap().is_playing, "a replug must not resume a manually paused (or call-paused) book");
        controller.stop();

        // Pause-on-unplug off: unplugging changes nothing at all.
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.set_headphone_behavior(false, true);
        start_book(&controller);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));
        controller.handle_route_event(unplug);
        assert!(controller.snapshot().unwrap().is_playing, "with pause-on-unplug off, an unplug must not pause");
        controller.handle_route_event(replug);
        assert!(controller.snapshot().unwrap().is_playing);
        controller.stop();

        // Route events with nothing loaded are no-ops by contract.
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.set_headphone_behavior(true, true);
        controller.handle_route_event(unplug);
        controller.handle_route_event(replug);
        assert!(controller.snapshot().is_none());
        controller.stop();
    }

    /// Regression test for the reported bug: toggling offline mode on Home didn't move Library's
    /// toggle or its filtered list until the app restarted (and vice versa), because each screen
    /// kept its own private `Rc<Cell<bool>>` instead of sharing one `OfflineModeState`. Builds
    /// both `screens::home::build` and `screens::library::build` sharing one `OfflineModeState`
    /// and one pool — exactly as `main_window::build` wires them — and toggles each screen's own
    /// widget in turn, asserting the *other*, already-built screen's toggle/banner/filtered list
    /// update live, with neither screen being rebuilt.
    pub(crate) fn run_offline_mode_toggle_is_shared_between_home_and_library(runtime: &tokio::runtime::Runtime) {
        use crate::screens::home::tests::{account_and_server, count_children, item_json};
        use crate::screens::library::tests::flow_box_titles;

        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/libraries"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "libraries": [{ "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", "name": "Audiobooks", "mediaType": "book" }]
                })))
                .mount(&mock_server),
        );
        runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [item_json("item-1", "Project Hail Mary"), item_json("item-2", "Dune")]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);

        let offline_mode = crate::offline_mode::OfflineModeState::new(pool.clone());

        let home_screen = crate::screens::home::build(
            pool.clone(),
            crate::test_support::test_paths(),
            server.clone(),
            account.clone(),
            session.clone(),
            offline_mode.clone(),
            |_| {},
            || {},
            |_| {},
        );
        let library_screen = crate::screens::library::build(
            pool.clone(),
            crate::test_support::test_paths(),
            server.clone(),
            account.clone(),
            session,
            offline_mode,
            |_| {},
            || {},
        );
        let home_hooks = home_screen.test_hooks();
        let library_hooks = library_screen.test_hooks();

        pump_until(|| count_children(&home_hooks.recent_row) == 2, std::time::Duration::from_secs(10));
        pump_until(|| library_hooks.flow_box.child_at_index(1).is_some(), std::time::Duration::from_secs(10));

        // Mark "item-1" downloaded, same setup as each screen's own single-screen offline test.
        runtime.block_on(abs_storage::repo::tracks::upsert_all(&pool, &server.id, "item-1", &[abs_storage::repo::tracks::NewTrack { ino: "1", duration_seconds: 3600.0, offset_seconds: 0.0, size_bytes: None }])).unwrap();
        runtime.block_on(abs_storage::repo::download_tracks::upsert_pending(&pool, &server.id, "item-1", "1", "/p/1.mp3")).unwrap();
        runtime.block_on(abs_storage::repo::download_tracks::mark_complete(&pool, &server.id, "item-1", "1", 10)).unwrap();

        // Toggle from HOME's widget only — Library must pick it up without being rebuilt.
        home_hooks.offline_toggle.set_active(true);
        pump_until(|| library_hooks.offline_toggle.is_active(), std::time::Duration::from_secs(5));
        assert!(library_hooks.offline_banner.reveals_child(), "Library's banner should reveal from a Home-driven toggle, with no rebuild");
        pump_until(|| flow_box_titles(&library_hooks.flow_box) == vec!["Project Hail Mary".to_string()], std::time::Duration::from_secs(5));
        assert!(home_hooks.offline_banner.reveals_child());

        // Toggle back off from LIBRARY's widget — Home must pick up the reverse.
        library_hooks.offline_toggle.set_active(false);
        pump_until(|| !home_hooks.offline_toggle.is_active(), std::time::Duration::from_secs(5));
        assert!(!home_hooks.offline_banner.reveals_child());
        pump_until(|| count_children(&home_hooks.recent_row) == 2, std::time::Duration::from_secs(5));
        assert_eq!(flow_box_titles(&library_hooks.flow_box).len(), 2);
    }

    /// The full chain restored by this plan: tapping a Home card opens Item Detail (not
    /// playback directly), Item Detail shows the tapped item's real metadata, and tapping its
    /// Play button both starts real playback and returns the shell (mini bar now visible) —
    /// same style as `run_offline_mode_toggle_is_shared_between_home_and_library`'s own
    /// cross-screen coverage.
    pub(crate) fn run_tapping_a_card_opens_item_detail_then_play_starts_playback_and_shows_the_shell_again(runtime: &tokio::runtime::Runtime) {
        use crate::screens::home::tests::item_json;

        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/libraries"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "libraries": [{ "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", "name": "Audiobooks", "mediaType": "book" }]
                })))
                .mount(&mock_server),
        );
        runtime.block_on(
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [item_json("item-1", "Project Hail Mary")]
                })))
                .mount(&mock_server),
        );
        // Item Detail's own chapters/tracks resolve, and the real audio Play actually loads.
        runtime.block_on(crate::player::tests::mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(pool());
        let server_id = runtime.block_on(abs_storage::repo::servers::add(&pool, &mock_server.uri())).unwrap();
        let account_id = runtime.block_on(abs_storage::repo::accounts::add(&pool, &server_id, "jane", "token123", None)).unwrap();
        runtime.block_on(abs_storage::repo::accounts::set_active(&pool, &account_id)).unwrap();
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &account_id)).unwrap();

        let app_window = adw::ApplicationWindow::builder().build();
        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let servers_with_accounts = vec![(server.clone(), vec![account.clone()])];
        let window = build(
            pool,
            crate::test_support::test_paths(),
            server,
            account,
            session,
            abs_core::settings::PlaybackSettings::default(),
            abs_core::settings::Theme::default(),
            servers_with_accounts,
            app_window.clone(),
        );
        let hooks = window.test_hooks();

        app_window.present();
        pump_until(|| app_window.is_mapped(), std::time::Duration::from_secs(5));

        let home_root = hooks.stack.child_by_name("home").expect("home tab exists");
        pump_until(|| find_card_button(&home_root).is_some(), std::time::Duration::from_secs(10));
        let card_button = find_card_button(&home_root).expect("a synced item's card should render");
        card_button.emit_clicked();

        // Item Detail should now be the window's content, with the tapped item's real title —
        // proving the tap opened Detail rather than starting playback directly.
        pump_until(
            || app_window.content().is_some_and(|content| find_label_text(&content, "Project Hail Mary")),
            std::time::Duration::from_secs(5),
        );
        assert!(!hooks.mini_bar.bar.is_visible(), "opening Item Detail must not itself start playback");

        // Tapping Play should start real playback and hand control back to the shell.
        let content = app_window.content().expect("Item Detail should be showing");
        let play_button = find_button_labeled(&content, "Play").expect("Item Detail's Play button");
        play_button.emit_clicked();

        pump_until(|| hooks.mini_bar.bar.is_visible(), std::time::Duration::from_secs(10));
        assert!(
            app_window.content().is_some_and(|c| c == window.root),
            "tapping Play should restore the shell"
        );
        assert_eq!(hooks.mini_bar.title_label.label(), "Project Hail Mary", "the mini bar should reflect the item Play just started");
    }

    /// Depth-first search for the first `GtkButton` that wraps an `item_card::build` card —
    /// identified structurally (a `GtkBox` whose first child is a `GtkOverlay`, the cover
    /// overlay every card has), since none of these buttons carry a name/id to search by.
    fn find_card_button(root: &gtk4::Widget) -> Option<gtk4::Button> {
        fn walk(widget: &gtk4::Widget, found: &mut Option<gtk4::Button>) {
            if found.is_some() {
                return;
            }
            if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
                let is_card = button
                    .child()
                    .and_then(|child| child.downcast::<gtk4::Box>().ok())
                    .and_then(|card_box| card_box.first_child())
                    .and_then(|first| first.downcast::<gtk4::Overlay>().ok())
                    .is_some();
                if is_card {
                    *found = Some(button.clone());
                    return;
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                walk(&w, found);
                if found.is_some() {
                    return;
                }
                child = w.next_sibling();
            }
        }
        let mut found = None;
        walk(root, &mut found);
        found
    }

    /// Depth-first search for a `GtkLabel` with exactly this text anywhere under `root`.
    fn find_label_text(root: &gtk4::Widget, text: &str) -> bool {
        fn walk(widget: &gtk4::Widget, text: &str, found: &mut bool) {
            if *found {
                return;
            }
            if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                if label.label() == text {
                    *found = true;
                    return;
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                walk(&w, text, found);
                if *found {
                    return;
                }
                child = w.next_sibling();
            }
        }
        let mut found = false;
        walk(root, text, &mut found);
        found
    }

    /// Depth-first search for the first `GtkButton` with exactly this label anywhere under `root`.
    fn find_button_labeled(root: &gtk4::Widget, label: &str) -> Option<gtk4::Button> {
        fn walk(widget: &gtk4::Widget, label: &str, found: &mut Option<gtk4::Button>) {
            if found.is_some() {
                return;
            }
            if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
                if button.label().as_deref() == Some(label) {
                    *found = Some(button.clone());
                    return;
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                walk(&w, label, found);
                if found.is_some() {
                    return;
                }
                child = w.next_sibling();
            }
        }
        let mut found = None;
        walk(root, label, &mut found);
        found
    }
}
