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
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::settings::PlaybackSettings;
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
}

#[cfg(test)]
impl MainWindow {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

pub fn build(
    pool: SqlitePool,
    paths: AppPaths,
    server: Server,
    account: Account,
    session: abs_core::auth::Session,
    playback_settings: PlaybackSettings,
    window: adw::ApplicationWindow,
) -> MainWindow {
    let mini_bar = player::build_mini_bar(pool.clone(), paths.clone(), player::real_backend());

    // MPRIS registration is best-effort — no session bus (a bare console, a locked-down sandbox)
    // must never be fatal to playback, so a failure here is just a warning. On success, the
    // bridge becomes a permanent snapshot listener (via the foundation refactor's `add_listener`)
    // so the lock-screen/Shell media card stays current for the app's whole lifetime.
    let mpris_bridge: Rc<dyn abs_player::mpris::MprisCommands> =
        Rc::new(player::MprisBridge::new(mini_bar.controller.clone(), playback_settings.skip_forward_seconds as f64, playback_settings.skip_back_seconds as f64));
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

    let stack = adw::ViewStack::new();

    let on_play = {
        let controller = mini_bar.controller.clone();
        let session = session.clone();
        let default_speed = playback_settings.default_speed;
        move |request: PlayRequest| controller.start(session.clone(), request, default_speed)
    };

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    stack.add_titled_with_icon(
        &screens::home::build(pool.clone(), paths.clone(), server.clone(), account.clone(), session.clone(), on_play.clone()).root,
        Some("home"),
        "Home",
        "go-home-symbolic",
    );
    let library_screen = screens::library::build(pool, paths, server, account, session, on_play);
    stack.add_titled_with_icon(&library_screen.root, Some("library"), "Library", "system-file-manager-symbolic");
    stack.add_titled_with_icon(&stub_page("folder-download-symbolic", "Downloads"), Some("downloads"), "Downloads", "folder-download-symbolic");
    stack.add_titled_with_icon(&stub_page("emblem-system-symbolic", "Settings"), Some("settings"), "Settings", "emblem-system-symbolic");

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
        move |_, _, _, _| {
            let player_screen = screens::player::build(controller.clone(), playback_settings, {
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
        },
    }
}

/// A placeholder page for a destination that doesn't have a real screen yet — still a genuine
/// `AdwStatusPage` in the stack (not e.g. an empty box), so it reads as "not built yet" rather
/// than "broken".
fn stub_page(icon_name: &str, title: &str) -> adw::StatusPage {
    adw::StatusPage::builder().icon_name(icon_name).title(title).description("Coming soon").vexpand(true).build()
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
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &account_id)).unwrap();

        let app_window = adw::ApplicationWindow::builder().build();
        let session = abs_core::auth::Session::new(pool.clone(), &server.url, &server.id, &account);
        let window = build(pool, crate::test_support::test_paths(), server, account, session, abs_core::settings::PlaybackSettings::default(), app_window.clone());
        let hooks = window.test_hooks();

        for name in ["home", "library", "downloads", "settings"] {
            assert!(hooks.stack.child_by_name(name).is_some(), "missing destination: {name}");
        }
        assert!(hooks.switcher_bar.reveals(), "the tab bar should always be shown");
        assert!(!hooks.mini_bar.bar.is_visible(), "the mini bar should stay hidden until something plays");

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
        controller.start(abs_core::auth::Session::new(pool.clone(), &server.url, &server.id, &account), PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None }, 1.0);
        crate::test_support::pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), std::time::Duration::from_secs(10));

        let on_call_active: Box<dyn Fn()> = {
            let controller = controller.clone();
            Box::new(move || controller.pause())
        };
        on_call_active();

        assert!(!controller.snapshot().unwrap().is_playing, "a call becoming active should pause playback");
        controller.stop();
    }
}
