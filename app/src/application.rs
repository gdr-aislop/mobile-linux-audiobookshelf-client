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
