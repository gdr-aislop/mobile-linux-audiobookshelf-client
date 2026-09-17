mod application;
mod player;
mod screens;
#[cfg(test)]
mod test_support;
mod widgets;

use adw::prelude::*;
use application::AppState;

fn main() -> adw::glib::ExitCode {
    tracing_subscriber::fmt::init();

    // One shared Tokio runtime for the whole app (see the architecture plan's async-runtime
    // note): GTK runs on GLib's main loop, but abs-api/abs-storage are async. Setup that must
    // finish before the first window appears runs here with `block_on`; screens making live
    // network/DB calls afterwards (e.g. the Welcome screen's Connect button) spawn onto this same
    // runtime via `glib::spawn_future_local` instead.
    //
    // `_runtime_guard` MUST stay alive for the rest of `main` (not just across `block_on` above):
    // `enter()` sets the "current" Tokio runtime for whichever thread calls it, and
    // `spawn_future_local`-driven futures run on the GTK main thread, not inside `block_on`. Once
    // the guard is dropped, sqlx/reqwest calls made from those futures panic with "this
    // functionality requires a Tokio context" — caught by hand-driving the real Connect button
    // under Xvfb, since the widget test suite builds its own runtime/guard directly and never
    // exercises this file's setup.
    let runtime = tokio::runtime::Runtime::new().expect("build the Tokio runtime");
    let _runtime_guard = runtime.enter();
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

    AppState { pool, paths, active_account, playback_settings }
}

/// Every GTK-touching test across `screens::*` funnels through exactly these two `#[test]` fns.
/// `gtk4::init()` binds to whichever OS thread first calls it, and Rust's built-in test harness
/// gives every `#[test]` fn its own fresh OS thread even under `--test-threads=1` (that flag only
/// limits how many run *concurrently*, not which thread each runs on) — so two `#[test]` fns that
/// both call `gtk4::init()` panic with "Attempted to initialize GTK from two different threads" or
/// "Failed to acquire default main context", regardless of file. Each screen module keeps its own
/// test code local (`pub(crate) mod tests` with plain `run`/`run_live` functions, not `#[test]`s)
/// for readability; only the actual entry point lives here.
#[cfg(test)]
mod tests {
    #[test]
    fn gtk_fast_scenarios() {
        gtk4::init().expect("gtk4::init for this test");
        abs_player::init().expect("gstreamer::init for this test");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();

        crate::screens::welcome::tests::run(&runtime);
        crate::screens::home::tests::run_renders_synced_library_and_recently_added_item(&runtime);
        crate::screens::home::tests::run_shows_empty_state_when_the_server_has_no_libraries(&runtime);
        crate::screens::home::tests::run_shows_a_banner_when_sync_fails(&runtime);
        crate::screens::main_window::tests::run(&runtime);
        crate::player::tests::run_start_and_pause_persists_progress(&runtime);
        crate::player::tests::run_start_resumes_from_existing_progress(&runtime);
        crate::player::tests::run_multi_track_item_sets_a_caveat_note(&runtime);
        crate::player::tests::run_end_of_stream_pauses_and_marks_finished(&runtime);
        crate::player::tests::run_mini_bar_reflects_playback_state(&runtime);
        crate::player::tests::run_add_bookmark_persists_a_row(&runtime);
        crate::screens::player::tests::run(&runtime);
        crate::screens::player::tests::run_multi_track_caveat_is_shown(&runtime);
        crate::screens::player::tests::run_chapters_sheet_lists_and_seeks(&runtime);
        crate::screens::player::tests::run_speed_popover_changes_playback_speed(&runtime);
        crate::screens::player::tests::run_sleep_timer_end_of_chapter_pauses_at_the_boundary(&runtime);
        crate::screens::player::tests::run_add_bookmark_button_persists_a_row(&runtime);
        crate::widgets::cover_image::tests::run();
    }

    #[test]
    #[ignore]
    fn gtk_live_demo_server_scenarios() {
        gtk4::init().expect("gtk4::init for this test");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();

        crate::screens::welcome::tests::run_live(&runtime);
        crate::screens::home::tests::run_live(&runtime);
    }
}
