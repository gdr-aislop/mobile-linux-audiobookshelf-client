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
}

/// The user-facing version string. Sourced at compile time from this crate's manifest, which
/// itself inherits the workspace's single `[workspace.package].version` — there is exactly one
/// place a version number is written. Shared by the `--version`/`-v` command-line flags
/// (handled in `main` before any setup) and, eventually, the Settings screen's About row
/// (docs/design/ui-spec.md).
pub fn version_line() -> String {
    format!("Audiobookshelf {}", env!("CARGO_PKG_VERSION"))
}

pub fn build_application(state: AppState) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        build_window(app, &state);
    });

    app
}

fn build_window(app: &adw::Application, state: &AppState) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Audiobookshelf")
        .default_width(390)
        .default_height(760)
        .build();

    match &state.active_account {
        None => {
            let pool = state.pool.clone();
            let paths = state.paths.clone();
            let window_for_callback = window.clone();
            let on_success_pool = pool.clone();
            let playback_settings = state.playback_settings;
            let screen = screens::welcome::build(pool, move |added| {
                let pool = on_success_pool.clone();
                let paths = paths.clone();
                let window_for_callback = window_for_callback.clone();
                glib::spawn_future_local(async move {
                    // `add_server_and_login` only hands back ids; re-fetch the full rows rather
                    // than threading server/account fields through `AddedAccount` just for this.
                    let server = abs_storage::repo::servers::get(&pool, &added.server_id)
                        .await
                        .expect("the server just created by add_server_and_login must exist");
                    let account = abs_storage::repo::accounts::get(&pool, &added.account_id)
                        .await
                        .expect("the account just created by add_server_and_login must exist");
                    let main_window = screens::main_window::build(
                        pool,
                        paths,
                        server,
                        account,
                        playback_settings,
                        window_for_callback.clone(),
                    );
                    window_for_callback.set_content(Some(&main_window.root));
                });
            });
            window.set_content(Some(&screen.root));
        }
        Some(account) => {
            let pool = state.pool.clone();
            let paths = state.paths.clone();
            let account = account.clone();
            let window_for_callback = window.clone();
            let playback_settings = state.playback_settings;
            glib::spawn_future_local(async move {
                let server = abs_storage::repo::servers::get(&pool, &account.server_id)
                    .await
                    .expect("an active account's server must exist");
                let main_window = screens::main_window::build(
                    pool,
                    paths,
                    server,
                    account,
                    playback_settings,
                    window_for_callback.clone(),
                );
                window_for_callback.set_content(Some(&main_window.root));
            });
        }
    }

    window.present();
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
