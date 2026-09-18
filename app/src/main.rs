mod application;
mod downloads;
mod player;
mod screens;
#[cfg(test)]
mod test_support;
mod widgets;

use adw::prelude::*;
use application::AppState;

fn main() -> adw::glib::ExitCode {
    // `--version` / `-v`: print and exit before *any* side effect — notably before setup()
    // resolves XDG paths and opens/migrates the user's database, which a version query has no
    // business doing. Parsed by hand here rather than via GApplication's option machinery
    // (`add_main_option` + `handle-local-options`) because that only runs inside `app.run()`,
    // after this file's eager `setup()` has already happened. Nothing below this point runs
    // for the flag: no Tokio runtime, no GStreamer, no disk access.
    if std::env::args().any(|arg| arg == "--version" || arg == "-v") {
        println!("{}", application::version_line());
        return 0.into();
    }

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
    // Token freshness is handled lazily by `abs_core::auth::Session` — screens ask it for a
    // token at call time, and its first use refreshes an expired one (server v2.26.0+ JWTs).

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
        crate::screens::home::tests::run_shows_a_retryable_error_when_the_first_sync_fails(&runtime);
        crate::screens::home::tests::run_offline_mode_toggle_filters_recently_added(&runtime);
        crate::screens::home::tests::run_shows_a_spinner_while_the_first_sync_is_running(&runtime);
        crate::screens::home::tests::run_expired_token_is_refreshed_before_syncing(&runtime);
        crate::screens::library::tests::run_renders_all_synced_items(&runtime);
        crate::screens::library::tests::run_search_filters_by_title_and_author(&runtime);
        crate::screens::library::tests::run_sort_changes_order(&runtime);
        crate::screens::library::tests::run_offline_mode_toggle_filters_to_downloaded_items(&runtime);
        crate::screens::library::tests::run_shows_a_banner_when_sync_fails(&runtime);
        crate::screens::library::tests::run_shows_empty_state_when_the_server_has_no_libraries(&runtime);
        crate::screens::library::tests::run_tapping_a_card_invokes_on_play(&runtime);
        crate::screens::library::tests::run_list_view_toggle_switches_visible_container(&runtime);
        crate::screens::library::tests::run_list_view_rows_show_title_and_subtitle(&runtime);
        crate::screens::library::tests::run_tapping_a_list_row_invokes_on_play(&runtime);
        crate::screens::library::tests::run_search_and_sort_apply_in_list_mode_too(&runtime);
        crate::screens::library::tests::run_view_mode_is_remembered_across_screen_rebuilds(&runtime);
        crate::widgets::item_card::tests::run();
        crate::widgets::item_card::tests::run_wrap_title_shows_the_full_title_without_ellipsizing();
        crate::widgets::item_card::tests::run_downloaded_badge_shows_only_when_downloaded();
        crate::screens::main_window::tests::run(&runtime);
        crate::screens::main_window::tests::run_call_interruption_wiring_pauses_playback(&runtime);
        crate::player::tests::run_start_and_pause_persists_progress(&runtime);
        crate::player::tests::run_downloaded_track_is_preferred_over_streaming(&runtime);
        crate::player::tests::run_untrustworthy_complete_row_falls_back_to_streaming(&runtime);
        crate::player::tests::run_multi_track_mixed_downloaded_and_streamed(&runtime);
        crate::player::tests::run_start_resumes_from_existing_progress(&runtime);
        crate::player::tests::run_track_duration_correction_updates_the_book_total(&runtime);
        crate::player::tests::run_multi_track_advances_to_the_next_track(&runtime);
        crate::player::tests::run_multi_track_final_track_marks_finished(&runtime);
        crate::player::tests::run_seek_across_track_boundary_lands_in_the_next_file(&runtime);
        crate::player::tests::run_resume_jumps_straight_to_the_second_track(&runtime);
        crate::player::tests::run_end_of_stream_pauses_and_marks_finished(&runtime);
        crate::player::tests::run_mini_bar_reflects_playback_state(&runtime);
        crate::player::tests::run_add_bookmark_persists_a_row(&runtime);
        crate::screens::player::tests::run(&runtime);
        crate::screens::player::tests::run_keyboard_actions(&runtime);
        crate::screens::player::tests::run_chapters_sheet_lists_and_seeks(&runtime);
        crate::screens::player::tests::run_download_button_starts_a_download_and_reflects_state(&runtime);
        crate::screens::player::tests::run_speed_popover_changes_playback_speed(&runtime);
        crate::screens::player::tests::run_sleep_timer_end_of_chapter_pauses_at_the_boundary(&runtime);
        crate::screens::player::tests::run_add_bookmark_button_persists_a_row(&runtime);
        crate::screens::player::tests::run_mark_as_finished_button_updates_progress(&runtime);
        crate::screens::player::tests::run_reset_progress_button_resets_position_and_seeks(&runtime);
        crate::widgets::cover_image::tests::run();
        crate::downloads::tests::run_start_download_fetches_only_the_needed_track_and_reaches_complete(&runtime);
        crate::downloads::tests::run_cancel_item_stops_the_track_from_reaching_complete(&runtime);
        crate::downloads::tests::run_wifi_only_blocks_a_download_on_a_metered_connection(&runtime);
        crate::downloads::tests::run_clear_item_deletes_files_and_publishes_idle(&runtime);
        crate::downloads::tests::run_start_download_with_no_chapters_fetches_every_track(&runtime);
        crate::downloads::tests::run_set_wifi_only_only_affects_downloads_started_afterwards(&runtime);
        crate::screens::downloads::tests::run_empty_state_renders_when_nothing_is_downloaded(&runtime);
        crate::screens::downloads::tests::run_in_progress_download_shows_a_cancel_row(&runtime);
        crate::screens::downloads::tests::run_completed_download_can_be_removed(&runtime);
    }

    #[test]
    #[ignore]
    fn gtk_live_demo_server_scenarios() {
        gtk4::init().expect("gtk4::init for this test");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();

        crate::screens::welcome::tests::run_live(&runtime);
        crate::screens::home::tests::run_live(&runtime);
        crate::screens::library::tests::run_live(&runtime);
    }
}
