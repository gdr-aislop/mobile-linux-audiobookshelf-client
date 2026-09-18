//! The GTK application shell. Decides whether to show the Welcome/login screen or the main
//! post-login window (`screens::main_window`) based on whether an account is active, and hosts
//! the single `AdwApplicationWindow` both get swapped into via `set_content`.

use abs_storage::paths::APP_ID;
use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use crate::screens;

/// Everything the UI needs a handle to. Constructed once in `main` after the async setup step
/// (DB connect + migrate, active-account lookup) completes, then moved into the
/// `connect_activate` closure.
pub struct AppState {
    pub pool: SqlitePool,
    pub paths: abs_storage::AppPaths,
    pub active_account: Option<abs_storage::models::Account>,
    pub playback_settings: abs_core::settings::PlaybackSettings,
    pub theme: abs_core::settings::Theme,
}

/// The user-facing version string. Sourced at compile time from this crate's manifest, which
/// itself inherits the workspace's single `[workspace.package].version` — there is exactly one
/// place a version number is written. Shared by the `--version`/`-v` command-line flags
/// (handled in `main` before any setup) and, eventually, the Settings screen's About row
/// (docs/design/ui-spec.md).
pub fn version_line() -> String {
    format!("Audiobookshelf {}", env!("CARGO_PKG_VERSION"))
}

/// Applies a Theme to libadwaita's global style manager — the one place that knows the
/// setting-to-scheme mapping. Called once at startup (before the first window builds) and live
/// from the Settings screen's Theme row.
pub fn apply_theme(theme: abs_core::settings::Theme) {
    let scheme = match theme {
        abs_core::settings::Theme::System => adw::ColorScheme::Default,
        abs_core::settings::Theme::Light => adw::ColorScheme::ForceLight,
        abs_core::settings::Theme::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

pub fn build_application(state: AppState) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        build_window(app, &state);
    });

    app
}

fn build_window(app: &adw::Application, state: &AppState) {
    // Before any window content builds — the theme is a global, so the Welcome screen (and every
    // sheet/popover) gets it too, not just the signed-in shell.
    apply_theme(state.theme);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Audiobookshelf")
        .default_width(390)
        .default_height(760)
        .build();

    add_keyboard_support(app, &window);

    match &state.active_account {
        None => show_welcome(&window, state.pool.clone(), state.paths.clone(), state.playback_settings, None),
        Some(_) => show_main(&window, state.pool.clone(), state.paths.clone(), state.playback_settings),
    }

    window.present();
}

/// Swaps the window's content for the login screen. `seed` turns this into the re-login flow:
/// the form comes pre-filled with the seeded session's URL and username (only the password left
/// to type), a Cancel affordance returns to the main window, and replacing the seeded session's
/// local data with different credentials asks for confirmation first. `None` is the plain
/// first-run flow.
pub(crate) fn show_welcome(
    window: &adw::ApplicationWindow,
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    playback_settings: abs_core::settings::PlaybackSettings,
    seed: Option<abs_core::accounts::ReloginSeed>,
) {
    let window_for_callback = window.clone();
    let on_success_pool = pool.clone();
    let on_success_paths = paths.clone();
    // Cancel only exists in the re-login flow (first run has nothing to go back to), and goes
    // back to the main window resolved fresh from the database — the session it returns to is
    // exactly the seeded one, untouched by an aborted re-login.
    let on_cancel: Option<std::rc::Rc<dyn Fn()>> = seed.as_ref().map(|_| {
        let pool = pool.clone();
        let paths = paths.clone();
        let window = window.clone();
        std::rc::Rc::new(move || show_main(&window, pool.clone(), paths.clone(), playback_settings))
            as std::rc::Rc<dyn Fn()>
    });

    let screen = screens::welcome::build(
        pool,
        paths,
        seed,
        move |_added| {
            // The just-logged-in account is the active one by the time on_success fires, so
            // resolving the main window from the DB's active account (rather than the callback's
            // ids) builds the identical shell — and doubles as the Cancel flow's entry point.
            show_main(&window_for_callback, on_success_pool.clone(), on_success_paths.clone(), playback_settings);
        },
        on_cancel,
    );
    window.set_content(Some(&screen.root));
}

/// "Add Server" from Settings' Servers group: the plain first-run login flow (`previous=None`
/// runs `add_server_and_login`), shown over the signed-in shell with a Cancel affordance back
/// to it — the Welcome screen only drops its Cancel button when no way back exists, so one is
/// provided here. A successful connect makes the new account the active one and rebuilds the
/// shell from the database exactly like every other session change.
pub(crate) fn show_add_server(
    window: &adw::ApplicationWindow,
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    playback_settings: abs_core::settings::PlaybackSettings,
) {
    // Captured before the swap: Cancel restores this exact widget, so the shell keeps its state
    // (open tab, scroll positions) — the same policy as collapsing the full player. A successful
    // connect, by contrast, rebuilds: the new account is the active one and the old shell's
    // session is stale.
    let on_cancel: std::rc::Rc<dyn Fn()> = match window.content() {
        Some(previous_root) => {
            let window = window.clone();
            std::rc::Rc::new(move || window.set_content(Some(&previous_root)))
        }
        None => {
            let pool = pool.clone();
            let paths = paths.clone();
            let window = window.clone();
            std::rc::Rc::new(move || show_main(&window, pool.clone(), paths.clone(), playback_settings))
        }
    };
    let on_success_pool = pool.clone();
    let on_success_paths = paths.clone();
    let on_success_window = window.clone();
    let screen = screens::welcome::build(
        pool,
        paths,
        None,
        move |_added| show_main(&on_success_window, on_success_pool.clone(), on_success_paths.clone(), playback_settings),
        Some(on_cancel),
    );
    window.set_content(Some(&screen.root));
}

/// The single entry point after any session mutation from Settings (switch server, sign out,
/// remove server): resolves the database's active account and swaps the window's content for
/// the main shell when one exists, or the plain first-run Welcome screen when none does (the
/// last account signed out or the last server removed). The old shell — and with it any
/// ongoing playback — is dropped by the swap; ending the session that was playing is the
/// point of signing out, not a side effect to engineer around.
pub(crate) fn show_main_or_welcome(
    window: &adw::ApplicationWindow,
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    playback_settings: abs_core::settings::PlaybackSettings,
) {
    let window_for_callback = window.clone();
    glib::spawn_future_local(async move {
        let active = abs_storage::repo::accounts::get_active(&pool)
            .await
            .expect("checking for an active account must not fail");
        match active {
            Some(_) => {
                let main_window =
                    build_main_window(pool, paths, playback_settings, window_for_callback.clone())
                        .await;
                window_for_callback.set_content(Some(&main_window.root));
            }
            None => show_welcome(&window_for_callback, pool, paths, playback_settings, None),
        }
    });
}

/// Swaps the window's content for the main shell, resolved from the database's active account —
/// the single entry point for every path back into the app (startup, post-login, re-login
/// cancel).
pub(crate) fn show_main(
    window: &adw::ApplicationWindow,
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    playback_settings: abs_core::settings::PlaybackSettings,
) {
    let window_for_content = window.clone();
    glib::spawn_future_local(async move {
        let main_window = build_main_window(pool, paths, playback_settings, window_for_content.clone()).await;
        window_for_content.set_content(Some(&main_window.root));
    });
}

/// Resolves the active account (and its server) from storage and builds the main shell. Panics
/// on a missing active account — every caller guarantees one by construction (checked at startup,
/// or just created by a successful login), so this is a caller bug, not a runtime condition.
async fn build_main_window(
    pool: SqlitePool,
    paths: abs_storage::AppPaths,
    playback_settings: abs_core::settings::PlaybackSettings,
    window: adw::ApplicationWindow,
) -> screens::main_window::MainWindow {
    // Re-fetch the full rows rather than threading server/account fields through callbacks —
    // ids are all the login flows hand back.
    let account = abs_storage::repo::accounts::get_active(&pool)
        .await
        .expect("checking for an active account must not fail")
        .expect("the main window requires an active account");
    let server = abs_storage::repo::servers::get(&pool, &account.server_id)
        .await
        .expect("an active account's server must exist");
    let theme = abs_core::settings::load_theme(&pool).await.expect("load theme");
    let session = abs_core::auth::Session::new(pool.clone(), &server.url, &server.id, &account);
    let servers_with_accounts = fetch_servers_with_accounts(&pool).await;
    screens::main_window::build(
        pool,
        paths,
        server,
        account,
        session,
        playback_settings,
        theme,
        servers_with_accounts,
        window,
    )
}

/// Every configured server with its accounts, in creation order — the data behind Settings'
/// Account and Servers groups. Each server's account list is fetched separately (a join would
/// hand back flat rows to regroup anyway).
pub(crate) async fn fetch_servers_with_accounts(pool: &SqlitePool) -> Vec<(abs_storage::models::Server, Vec<abs_storage::models::Account>)> {
    let servers = abs_storage::repo::servers::list(pool).await.expect("listing servers must not fail");
    let mut result = Vec::with_capacity(servers.len());
    for server in servers {
        let accounts = abs_storage::repo::accounts::list_for_server(pool, &server.id)
            .await
            .expect("listing a server's accounts must not fail");
        result.push((server, accounts));
    }
    result
}

/// The keyboard half of ui-spec §6: the accelerators (app-wide — they only fire when the matching
/// action actually exists, so `player.*` accels are inert until the full player merges its action
/// group, and `win.*` ones don't exist before login) and the `Ctrl+?` shortcuts overlay. The
/// actions themselves live where their state is: `win.*` in `screens::main_window`, `player.*` on
/// the full player screen.
fn add_keyboard_support(app: &adw::Application, window: &adw::ApplicationWindow) {
    let quit_action = adw::gio::SimpleAction::new("quit", None);
    quit_action.connect_activate({
        let app = app.clone();
        move |_, _| app.quit()
    });
    app.add_action(&quit_action);

    window.set_help_overlay(Some(&build_shortcuts_overlay()));

    for (action, accels) in [
        ("app.quit", vec!["<Control>q"]),
        // GtkApplicationWindow also wires this action itself; stating the accel keeps the
        // binding visible and independent of that internal default.
        ("win.show-help-overlay", vec!["<Control>question"]),
        ("win.switch-tab('home')", vec!["<Alt>1"]),
        ("win.switch-tab('library')", vec!["<Alt>2"]),
        ("win.switch-tab('downloads')", vec!["<Alt>3"]),
        ("win.switch-tab('settings')", vec!["<Alt>4", "<Control>comma"]),
        ("win.open-library-search", vec!["<Control>f"]),
        ("win.play-pause", vec!["space"]),
        ("win.bookmark", vec!["b"]),
        ("player.skip-back", vec!["Left"]),
        ("player.skip-forward", vec!["Right"]),
        // Dual bindings (with and without Ctrl) match Decibels, GNOME's own audio player and
        // the closest analogue for these controls.
        ("player.speed-up", vec!["<Control>plus", "plus"]),
        ("player.speed-down", vec!["<Control>minus", "minus"]),
        ("player.speed-reset", vec!["<Control>0", "0"]),
        ("player.chapters", vec!["c"]),
        ("player.sleep-timer", vec!["t"]),
        ("player.collapse", vec!["Escape"]),
    ] {
        app.set_accels_for_action(action, &accels);
    }
}

/// The `GtkShortcutsWindow` behind `Ctrl+?` — one item per row of ui-spec §6's tables, grouped
/// the same way.
///
/// Built from an inline `GtkBuilder` definition rather than the Rust widget bindings: the
/// `ShortcutsWindow` family's `add_section`/`add_group`/`add_shortcut` methods are gated behind
/// gtk4-rs's `v4_14` feature, and this crate deliberately pins `v4_8` as its API ceiling (see
/// `app/Cargo.toml` — loosening it for three methods would let future code reach real 4.14-only
/// APIs by accident). The underlying C symbols are ancient (GTK 4.0), so the runtime library on
/// PureOS Crimson handles this fine; a declarative definition is also the form upstream apps
/// themselves use for this widget (Decibels ships its shortcuts dialog as a `.ui` file).
fn build_shortcuts_overlay() -> gtk4::ShortcutsWindow {
    const DEFINITION: &str = r#"
<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="overlay">
    <child>
      <object class="GtkShortcutsSection">
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Show shortcuts</property>
                <property name="accelerator">&lt;Control&gt;question</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Quit</property>
                <property name="accelerator">&lt;Control&gt;q</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Navigation</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Go to Home</property>
                <property name="accelerator">&lt;Alt&gt;1</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Go to Library</property>
                <property name="accelerator">&lt;Alt&gt;2</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Go to Downloads</property>
                <property name="accelerator">&lt;Alt&gt;3</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Go to Settings</property>
                <property name="accelerator">&lt;Alt&gt;4 / &lt;Control&gt;comma</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Search the library</property>
                <property name="accelerator">&lt;Control&gt;f</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Playback</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Play / pause</property>
                <property name="accelerator">space</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Add bookmark</property>
                <property name="accelerator">b</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Full player</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Skip back / forward</property>
                <property name="accelerator">Left / Right</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Speed up / down</property>
                <property name="accelerator">&lt;Control&gt;plus / &lt;Control&gt;minus</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Reset speed to 1×</property>
                <property name="accelerator">&lt;Control&gt;0</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Open chapters</property>
                <property name="accelerator">c</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Open sleep timer</property>
                <property name="accelerator">t</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Collapse player</property>
                <property name="accelerator">Escape</property>
              </object>
            </child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>
"#;

    let builder = gtk4::Builder::from_string(DEFINITION);
    builder.object::<gtk4::ShortcutsWindow>("overlay").expect("the shortcuts overlay definition must contain its root object")
}

#[cfg(test)]
mod tests {
    use super::version_line;

    #[test]
    fn version_line_is_the_display_name_plus_a_dotted_version() {
        let line = version_line();
        let version = line
            .strip_prefix("Audiobookshelf ")
            .expect("version line should start with the display name");
        assert!(!version.is_empty(), "version must not be empty");
        let parts: Vec<&str> = version.split('.').collect();
        assert!(parts.len() >= 2, "version should be dotted, got: {version}");
        assert!(
            parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "version should be numeric and dotted, got: {version}"
        );
    }
}
