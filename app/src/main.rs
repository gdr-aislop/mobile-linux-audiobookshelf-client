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

/// Every GTK-touching test across `screens::*` funnels through this module. `gtk4::init()` binds
/// to whichever OS thread first calls it, and Rust's built-in test harness gives every `#[test]`
/// fn its own fresh OS thread even under `--test-threads=1` (that flag only limits how many run
/// *concurrently*, not which thread each runs on) — so two `#[test]` fns that both call
/// `gtk4::init()` panic with "Attempted to initialize GTK from two different threads", regardless
/// of file. Each screen module therefore keeps its own test code local (`pub(crate) mod tests`
/// with plain `run_*` functions, not `#[test]`s) for readability; only the plumbing lives here.
///
/// The plumbing used to be one giant `#[test]` calling every scenario in sequence. That made the
/// suite all-or-nothing: a single flaky assert failed the whole ~45 s binary, and isolating one
/// scenario meant hand-editing this registry. Instead, every scenario below is generated as its
/// own `#[ignore]`d `#[test]`, and the one test cargo runs by default (`gtk_fast_scenarios`)
/// re-execs *this test binary* once per scenario with `--exact --ignored` — so each scenario gets
/// a fresh process (its own GTK init, main context, GStreamer, tempdirs), a named pass/fail, and
/// its failure name doubles as the command to re-run it in isolation:
///
///     cargo test -p abs-app -- --exact tests::home_offline_mode_toggle_filters_recently_added --ignored
///
/// (The per-scenario tests can't run under a plain `cargo test` themselves — that's a second
/// GTK init on a second thread; they exist to be picked one-per-process by the driver or by hand.)
#[cfg(test)]
mod tests {
    /// Boots one scenario in "a process that owns GTK": init, then a shared Tokio runtime whose
    /// thread-local guard must outlive the scenario (see `main`'s runtime-guard comment — same
    /// requirement, same failure mode if dropped early: sqlx/reqwest calls made from
    /// `glib::spawn_future_local` futures panic with "requires a Tokio context").
    fn run_scenario(scenario: fn(&tokio::runtime::Runtime)) {
        gtk4::init().expect("gtk4::init for the scenario process");
        abs_player::init().expect("gstreamer::init for the scenario process");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        scenario(&runtime);
    }

    /// One entry per scenario: `(name, closure)`. The closure receives the shared `&Runtime`
    /// (scenarios that don't need one ignore it as `_rt`). `name` becomes the generated
    /// `#[test]` fn and the `tests::<name>` handle both the driver and manual re-runs use —
    /// keep names unique and module-prefixed, since several screens share scenario tails like
    /// "…shows_empty_state_when_the_server_has_no_libraries".
    macro_rules! gtk_scenarios {
        ($(($name:ident, $call:expr)),* $(,)?) => {
            $(
                #[test]
                #[ignore = "per-scenario process: run via the gtk_fast_scenarios driver, or --exact --ignored tests::<name>"]
                fn $name() {
                    run_scenario($call);
                }
            )*

            /// Walked by the `gtk_fast_scenarios` driver; kept in registry order for readable output.
            pub(crate) const GTK_SCENARIOS: &[&str] = &[$(stringify!($name)),*];
        };
    }

    gtk_scenarios! {
        (welcome_connect, |rt| crate::screens::welcome::tests::run(rt)),
        (welcome_relogin_flow, |rt| crate::screens::welcome::tests::run_relogin_flow(rt)),
        (home_renders_synced_library_and_recently_added_item, |rt| crate::screens::home::tests::run_renders_synced_library_and_recently_added_item(rt)),
        (home_finished_books_dont_show_under_continue_listening, |rt| crate::screens::home::tests::run_finished_books_dont_show_under_continue_listening(rt)),
        (home_shows_empty_state_when_the_server_has_no_libraries, |rt| crate::screens::home::tests::run_shows_empty_state_when_the_server_has_no_libraries(rt)),
        (home_shows_a_retryable_error_when_the_first_sync_fails, |rt| crate::screens::home::tests::run_shows_a_retryable_error_when_the_first_sync_fails(rt)),
        (home_offline_mode_toggle_filters_recently_added, |rt| crate::screens::home::tests::run_offline_mode_toggle_filters_recently_added(rt)),
        (home_shows_a_spinner_while_the_first_sync_is_running, |rt| crate::screens::home::tests::run_shows_a_spinner_while_the_first_sync_is_running(rt)),
        (home_expired_token_is_refreshed_before_syncing, |rt| crate::screens::home::tests::run_expired_token_is_refreshed_before_syncing(rt)),
        (home_offers_login_again_when_the_session_is_rejected, |rt| crate::screens::home::tests::run_offers_login_again_when_the_session_is_rejected(rt)),
        (home_shows_login_again_in_the_banner_when_a_resync_is_rejected, |rt| crate::screens::home::tests::run_shows_login_again_in_the_banner_when_a_resync_is_rejected(rt)),
        (library_renders_all_synced_items, |rt| crate::screens::library::tests::run_renders_all_synced_items(rt)),
        (library_search_filters_by_title_and_author, |rt| crate::screens::library::tests::run_search_filters_by_title_and_author(rt)),
        (library_sort_changes_order, |rt| crate::screens::library::tests::run_sort_changes_order(rt)),
        (library_offline_mode_toggle_filters_to_downloaded_items, |rt| crate::screens::library::tests::run_offline_mode_toggle_filters_to_downloaded_items(rt)),
        (library_shows_a_banner_when_sync_fails, |rt| crate::screens::library::tests::run_shows_a_banner_when_sync_fails(rt)),
        (library_shows_login_again_in_the_banner_when_a_resync_is_rejected, |rt| crate::screens::library::tests::run_shows_login_again_in_the_banner_when_a_resync_is_rejected(rt)),
        (library_shows_empty_state_when_the_server_has_no_libraries, |rt| crate::screens::library::tests::run_shows_empty_state_when_the_server_has_no_libraries(rt)),
        (library_tapping_a_card_invokes_on_play, |rt| crate::screens::library::tests::run_tapping_a_card_invokes_on_play(rt)),
        (library_list_view_toggle_switches_visible_container, |rt| crate::screens::library::tests::run_list_view_toggle_switches_visible_container(rt)),
        (library_list_view_rows_show_title_and_subtitle, |rt| crate::screens::library::tests::run_list_view_rows_show_title_and_subtitle(rt)),
        (library_tapping_a_list_row_invokes_on_play, |rt| crate::screens::library::tests::run_tapping_a_list_row_invokes_on_play(rt)),
        (library_search_and_sort_apply_in_list_mode_too, |rt| crate::screens::library::tests::run_search_and_sort_apply_in_list_mode_too(rt)),
        (library_view_mode_is_remembered_across_screen_rebuilds, |rt| crate::screens::library::tests::run_view_mode_is_remembered_across_screen_rebuilds(rt)),
        (item_card_renders, |_rt| crate::widgets::item_card::tests::run()),
        (item_card_wrap_title_shows_the_full_title_without_ellipsizing, |_rt| crate::widgets::item_card::tests::run_wrap_title_shows_the_full_title_without_ellipsizing()),
        (item_card_downloaded_badge_shows_only_when_downloaded, |_rt| crate::widgets::item_card::tests::run_downloaded_badge_shows_only_when_downloaded()),
        (main_window_shell, |rt| crate::screens::main_window::tests::run(rt)),
        (main_window_call_interruption_wiring_pauses_playback, |rt| crate::screens::main_window::tests::run_call_interruption_wiring_pauses_playback(rt)),
        (main_window_headphone_route_events_follow_the_settings, |rt| crate::screens::main_window::tests::run_headphone_route_events_follow_the_settings(rt)),
        (settings_persistence, |rt| crate::screens::settings::tests::run(rt)),
        (playback_start_and_pause_persists_progress, |rt| crate::player::tests::run_start_and_pause_persists_progress(rt)),
        (playback_downloaded_track_is_preferred_over_streaming, |rt| crate::player::tests::run_downloaded_track_is_preferred_over_streaming(rt)),
        (playback_untrustworthy_complete_row_falls_back_to_streaming, |rt| crate::player::tests::run_untrustworthy_complete_row_falls_back_to_streaming(rt)),
        (playback_multi_track_mixed_downloaded_and_streamed, |rt| crate::player::tests::run_multi_track_mixed_downloaded_and_streamed(rt)),
        (playback_start_resumes_from_existing_progress, |rt| crate::player::tests::run_start_resumes_from_existing_progress(rt)),
        (playback_track_duration_correction_updates_the_book_total, |rt| crate::player::tests::run_track_duration_correction_updates_the_book_total(rt)),
        (playback_multi_track_advances_to_the_next_track, |rt| crate::player::tests::run_multi_track_advances_to_the_next_track(rt)),
        (playback_multi_track_final_track_marks_finished, |rt| crate::player::tests::run_multi_track_final_track_marks_finished(rt)),
        (playback_seek_across_track_boundary_lands_in_the_next_file, |rt| crate::player::tests::run_seek_across_track_boundary_lands_in_the_next_file(rt)),
        (playback_resume_jumps_straight_to_the_second_track, |rt| crate::player::tests::run_resume_jumps_straight_to_the_second_track(rt)),
        (playback_end_of_stream_pauses_and_marks_finished, |rt| crate::player::tests::run_end_of_stream_pauses_and_marks_finished(rt)),
        (playback_mini_bar_reflects_playback_state, |rt| crate::player::tests::run_mini_bar_reflects_playback_state(rt)),
        (playback_add_bookmark_persists_a_row, |rt| crate::player::tests::run_add_bookmark_persists_a_row(rt)),
        (player_screen_renders, |rt| crate::screens::player::tests::run(rt)),
        (player_screen_keyboard_actions, |rt| crate::screens::player::tests::run_keyboard_actions(rt)),
        (player_screen_chapters_sheet_lists_and_seeks, |rt| crate::screens::player::tests::run_chapters_sheet_lists_and_seeks(rt)),
        (player_screen_download_button_starts_a_download_and_reflects_state, |rt| crate::screens::player::tests::run_download_button_starts_a_download_and_reflects_state(rt)),
        (player_screen_speed_popover_changes_playback_speed, |rt| crate::screens::player::tests::run_speed_popover_changes_playback_speed(rt)),
        (player_screen_sleep_timer_end_of_chapter_pauses_at_the_boundary, |rt| crate::screens::player::tests::run_sleep_timer_end_of_chapter_pauses_at_the_boundary(rt)),
        (player_screen_add_bookmark_button_persists_a_row, |rt| crate::screens::player::tests::run_add_bookmark_button_persists_a_row(rt)),
        (player_screen_mark_as_finished_button_updates_progress, |rt| crate::screens::player::tests::run_mark_as_finished_button_updates_progress(rt)),
        (player_screen_reset_progress_button_resets_position_and_seeks, |rt| crate::screens::player::tests::run_reset_progress_button_resets_position_and_seeks(rt)),
        (cover_image_rendering, |_rt| crate::widgets::cover_image::tests::run()),
        (downloads_engine_start_fetches_only_the_needed_track_and_reaches_complete, |rt| crate::downloads::tests::run_start_download_fetches_only_the_needed_track_and_reaches_complete(rt)),
        (downloads_engine_cancel_item_stops_the_track_from_reaching_complete, |rt| crate::downloads::tests::run_cancel_item_stops_the_track_from_reaching_complete(rt)),
        (downloads_engine_wifi_only_blocks_a_download_on_a_metered_connection, |rt| crate::downloads::tests::run_wifi_only_blocks_a_download_on_a_metered_connection(rt)),
        (downloads_engine_clear_item_deletes_files_and_publishes_idle, |rt| crate::downloads::tests::run_clear_item_deletes_files_and_publishes_idle(rt)),
        (downloads_engine_start_with_no_chapters_fetches_every_track, |rt| crate::downloads::tests::run_start_download_with_no_chapters_fetches_every_track(rt)),
        (downloads_engine_set_wifi_only_only_affects_downloads_started_afterwards, |rt| crate::downloads::tests::run_set_wifi_only_only_affects_downloads_started_afterwards(rt)),
        (downloads_screen_empty_state_renders_when_nothing_is_downloaded, |rt| crate::screens::downloads::tests::run_empty_state_renders_when_nothing_is_downloaded(rt)),
        (downloads_screen_in_progress_download_shows_a_cancel_row, |rt| crate::screens::downloads::tests::run_in_progress_download_shows_a_cancel_row(rt)),
        (downloads_screen_completed_download_can_be_removed, |rt| crate::screens::downloads::tests::run_completed_download_can_be_removed(rt)),
    }

    /// The only `#[test]` a plain `cargo test` runs in this binary: re-executes this same binary
    /// once per scenario so each gets its own GTK process — one flaky scenario can no longer fail
    /// the whole suite, and the failing scenario's name is its own isolation command.
    #[test]
    fn gtk_fast_scenarios() {
        let exe = std::env::current_exe().expect("locate this test binary");
        let mut failed = Vec::new();
        for name in GTK_SCENARIOS {
            // The child inherits our stdio, so its per-scenario output (panics, libtest's result
            // line) lands inline right here in the parent's log.
            let status = std::process::Command::new(&exe)
                .arg(format!("tests::{name}"))
                .args(["--exact", "--ignored"])
                .status()
                .expect("re-exec this test binary for the scenario process");
            if !status.success() {
                failed.push(*name);
            }
        }
        assert!(
            failed.is_empty(),
            "{}/{} scenarios failed: {:?}\nre-run one in isolation with: cargo test -p abs-app -- --exact tests::<name> --ignored",
            failed.len(),
            GTK_SCENARIOS.len(),
            failed,
        );
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
