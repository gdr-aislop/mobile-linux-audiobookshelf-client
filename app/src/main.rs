mod application;
mod screens;
mod widgets;

use adw::prelude::*;
use application::AppState;

fn main() -> adw::glib::ExitCode {
    tracing_subscriber::fmt::init();

    // One shared Tokio runtime for the whole app (see the architecture plan's async-runtime
    // note): GTK runs on GLib's main loop, but abs-api/abs-storage are async. Setup that must
    // finish before the first window appears runs here with `block_on`; once there are real
    // screens making live network calls, those will be spawned onto this runtime and marshaled
    // back to widgets via `glib::spawn_future_local` instead.
    let runtime = tokio::runtime::Runtime::new().expect("build the Tokio runtime");
    let state = runtime.block_on(setup());

    if let Err(err) = abs_player::init() {
        tracing::warn!(%err, "GStreamer failed to initialize; playback will be unavailable");
    }

    let app = application::build_application(state);
    app.run()
}

async fn setup() -> AppState {
    let paths = abs_storage::AppPaths::resolve().expect("resolve XDG application directories");
    paths.ensure_dirs().await.expect("create application directories");

    let pool = abs_storage::connect_and_migrate(&paths.db_path())
        .await
        .expect("open and migrate the local database");

    let playback_settings = abs_core::settings::load_playback_settings(&pool)
        .await
        .expect("load playback settings");
    tracing::info!(?playback_settings, "loaded playback settings");

    let active_account = abs_storage::repo::accounts::get_active(&pool)
        .await
        .expect("check for an active account");
    tracing::info!(
        has_active_account = active_account.is_some(),
        "resolved startup screen"
    );

    AppState { pool, paths, active_account }
}
