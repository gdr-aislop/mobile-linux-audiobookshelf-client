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

use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::settings::PlaybackSettings;
use abs_storage::models::{Account, Server};

use crate::player::{self, PlayRequest};
use crate::screens;

pub struct MainWindow {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub stack: adw::ViewStack,
    pub switcher_bar: adw::ViewSwitcherBar,
    pub mini_bar: crate::player::MiniPlayerHooks,
}

#[cfg(test)]
impl MainWindow {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

pub fn build(
    pool: SqlitePool,
    server: Server,
    account: Account,
    playback_settings: PlaybackSettings,
    window: adw::ApplicationWindow,
) -> MainWindow {
    let mini_bar = player::build_mini_bar(pool.clone(), player::real_backend());

    let stack = adw::ViewStack::new();

    let on_play = {
        let controller = mini_bar.controller.clone();
        let server = server.clone();
        let account = account.clone();
        move |request: PlayRequest| controller.start(server.clone(), account.clone(), request)
    };

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    stack.add_titled_with_icon(
        &screens::home::build(pool, server, account, on_play).root,
        Some("home"),
        "Home",
        "go-home-symbolic",
    );
    stack.add_titled_with_icon(&stub_page("system-file-manager-symbolic", "Library"), Some("library"), "Library", "system-file-manager-symbolic");
    stack.add_titled_with_icon(&stub_page("folder-download-symbolic", "Downloads"), Some("downloads"), "Downloads", "folder-download-symbolic");
    stack.add_titled_with_icon(&stub_page("emblem-system-symbolic", "Settings"), Some("settings"), "Settings", "emblem-system-symbolic");

    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).reveal(true).build();

    root.append(&stack);
    root.append(&mini_bar.root);
    root.append(&switcher_bar);

    // Tapping the mini bar opens the full player by swapping the window's content — there's no
    // `AdwNavigationView`/`AdwDialog` available at this crate's libadwaita ceiling (both v1.4+),
    // so this is the same content-swap mechanism `application.rs` already uses for Welcome -> main
    // window. Collapsing restores `root` (this shell), not a fresh `build()` call — no state lost.
    let mini_bar_gesture = gtk4::GestureClick::new();
    mini_bar_gesture.connect_released({
        let controller = mini_bar.controller.clone();
        let window = window.clone();
        let root = root.clone();
        move |_, _, _, _| {
            let player_screen = screens::player::build(controller.clone(), playback_settings, {
                let window = window.clone();
                let root = root.clone();
                move || window.set_content(Some(&root))
            });
            window.set_content(Some(&player_screen.root));
        }
    });
    mini_bar.root.add_controller(mini_bar_gesture);

    MainWindow {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks { stack, switcher_bar, mini_bar: mini_bar.hooks },
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
    use crate::test_support::pool;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point. Points the Home tab's
    /// server at an address nothing listens on (`127.0.0.1:1`, immediate connection-refused) —
    /// this test only cares about the stack's shape, not sync behavior, so it must never make a
    /// real network call.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let server_id = runtime.block_on(abs_storage::repo::servers::add(&pool, "http://127.0.0.1:1")).unwrap();
        let account_id =
            runtime.block_on(abs_storage::repo::accounts::add(&pool, &server_id, "jane", "token")).unwrap();
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &account_id)).unwrap();

        let app_window = adw::ApplicationWindow::builder().build();
        let window = build(pool, server, account, abs_core::settings::PlaybackSettings::default(), app_window);
        let hooks = window.test_hooks();

        for name in ["home", "library", "downloads", "settings"] {
            assert!(hooks.stack.child_by_name(name).is_some(), "missing destination: {name}");
        }
        assert!(hooks.switcher_bar.reveals(), "the tab bar should always be shown");
        assert!(!hooks.mini_bar.bar.is_visible(), "the mini bar should stay hidden until something plays");
    }
}
