//! The Downloads tab — `docs/design/ui-spec.md`'s "Downloads" section: one row per item that has
//! been downloaded fully or partially, or is currently being fetched, with a remove/cancel action
//! and an empty state when nothing has been downloaded yet.
//!
//! Completed rows carry a gray subtitle — "10 chapters, 64.1 MB" — computed from the local
//! `download_tracks` rows (count of `Complete` tracks, summed `bytes_downloaded`). In-flight rows
//! show live progress instead: "3/10 chapters · 18.2 MB · 2.1 MB/s", driven by the manager's
//! `batch_progress` (chapters tick per track outcome) and `TrackProgress` events (bytes + a
//! smoothed speed), throttled so per-chunk events don't thrash the label — the spinner stays as
//! the at-a-glance "something is moving" cue.
//!
//! No storage-used/free-space summary row (the spec's own "nice to have") — computing device free
//! space needs `statvfs`-style OS calls with no existing precedent in this codebase, left as a
//! documented follow-up rather than built here.
//!
//! Never imports `abs_api`: the only calls here are `abs_core::download_tracks` (read the
//! downloaded-item list) and `DownloadManager` (cancel/clear) — same boundary every other screen
//! respects.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use sqlx::SqlitePool;

use crate::downloads::{DownloadEvent, DownloadManager, ItemDownloadState};
use abs_storage::models::DownloadStatus;

/// Minimum gap between live subtitle writes for one item — `TrackProgress` fires per chunk
/// written, which can be many times a second; the subtitle only needs ~5 updates a second.
const LABEL_UPDATE_INTERVAL: Duration = Duration::from_millis(200);

/// A speed sample older than this means nothing has arrived in a while — hide the speed part
/// rather than showing a frozen rate as if it were current.
const SPEED_DISPLAY_WINDOW: Duration = Duration::from_secs(2);

pub struct DownloadsScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub status_page: adw::StatusPage,
    pub list_box: gtk4::ListBox,
    pub scroller: gtk4::ScrolledWindow,
}

#[cfg(test)]
impl DownloadsScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

/// Per-item speed smoothing, kept across row rebuilds (rebuilds happen on every
/// `ItemStateChanged`; the download itself doesn't reset each time).
struct SpeedState {
    ema: f64,
    last_bytes: u64,
    last_instant: Option<Instant>,
    last_label_write: Instant,
}

struct Widgets {
    pool: SqlitePool,
    server_id: String,
    list_box: gtk4::ListBox,
    status_page: adw::StatusPage,
    scroller: gtk4::ScrolledWindow,
    download_manager: DownloadManager,
    /// Item ids this screen has itself observed going into `Downloading` — the persisted
    /// `downloaded_item_ids` query only ever reflects *complete* tracks, so a download still in
    /// flight (nothing complete yet) needs to be tracked here to show a row for it at all.
    downloading: Rc<RefCell<HashSet<String>>>,
    /// Per-item, per-track cumulative downloaded bytes — seeded from the DB at every refresh
    /// (`bytes_downloaded` is the full size for `Complete` tracks, the last checkpoint otherwise)
    /// and updated from `TrackProgress` events (per-track cumulative, so no deltas to reconcile).
    track_bytes: Rc<RefCell<HashMap<String, HashMap<String, u64>>>>,
    /// Smoothed per-item download speed across rebuilds — see `SpeedState`.
    speeds: Rc<RefCell<HashMap<String, SpeedState>>>,
    /// The in-flight rows themselves, keyed by item id — `TrackProgress` updates a row's subtitle
    /// text in place (throttled) instead of rebuilding the whole list per chunk. Rebuilt rows
    /// re-register here at refresh.
    live_rows: Rc<RefCell<HashMap<String, adw::ActionRow>>>,
}

pub fn build(
    pool: SqlitePool,
    _paths: abs_storage::AppPaths,
    server: abs_storage::models::Server,
    _account: abs_storage::models::Account,
    _session: abs_core::auth::Session,
    download_manager: DownloadManager,
) -> DownloadsScreen {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Downloads", "")));

    let status_page = adw::StatusPage::builder().icon_name("folder-download-symbolic").title("No downloads yet").vexpand(true).visible(false).build();

    let list_box = gtk4::ListBox::builder().selection_mode(gtk4::SelectionMode::None).css_classes(["boxed-list"]).margin_start(12).margin_end(12).margin_top(12).build();
    let scroller = gtk4::ScrolledWindow::builder().child(&list_box).vexpand(true).build();

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&status_page);
    root.append(&scroller);

    let widgets = Rc::new(Widgets {
        pool,
        server_id: server.id.clone(),
        list_box: list_box.clone(),
        status_page: status_page.clone(),
        scroller: scroller.clone(),
        download_manager: download_manager.clone(),
        downloading: Rc::new(RefCell::new(HashSet::new())),
        track_bytes: Rc::new(RefCell::new(HashMap::new())),
        speeds: Rc::new(RefCell::new(HashMap::new())),
        live_rows: Rc::new(RefCell::new(HashMap::new())),
    });

    // Registered once, for the manager's whole lifetime — same permanent-listener shape
    // `PlayerController::add_listener`/MPRIS already use. Every event fires on the GTK main
    // thread (the manager publishes from `spawn_future_local` futures only), so touching
    // widgets here is safe.
    download_manager.add_listener({
        let widgets = widgets.clone();
        move |event| match event {
            DownloadEvent::ItemStateChanged { item_id, state } => {
                match state {
                    ItemDownloadState::Downloading => {
                        widgets.downloading.borrow_mut().insert(item_id.clone());
                    }
                    ItemDownloadState::Idle | ItemDownloadState::Stopped | ItemDownloadState::Complete | ItemDownloadState::Failed => {
                        widgets.downloading.borrow_mut().remove(item_id);
                    }
                }
                spawn_refresh(widgets.clone());
            }
            DownloadEvent::TrackProgress { item_id, ino, bytes_downloaded, .. } => {
                // Per-track cumulative bytes: just overwrite the slot — no delta math.
                widgets.track_bytes.borrow_mut().entry(item_id.clone()).or_default().insert(ino.clone(), *bytes_downloaded);

                let now = Instant::now();
                let mut speeds = widgets.speeds.borrow_mut();
                let speed = speeds.entry(item_id.clone()).or_insert(SpeedState { ema: 0.0, last_bytes: *bytes_downloaded, last_instant: None, last_label_write: now });
                match speed.last_instant {
                    // First sample of a (re)started download: seed the baseline, no rate yet.
                    None => {
                        speed.last_bytes = *bytes_downloaded;
                        speed.last_instant = Some(now);
                    }
                    Some(_) => {
                        let elapsed = now.duration_since(speed.last_instant.unwrap()).as_secs_f64();
                        if elapsed > 0.0 {
                            let instant_rate = (*bytes_downloaded).saturating_sub(speed.last_bytes) as f64 / elapsed;
                            speed.ema = if speed.ema <= 0.0 { instant_rate } else { speed.ema * 0.5 + instant_rate * 0.5 };
                            speed.last_bytes = *bytes_downloaded;
                            speed.last_instant = Some(now);
                        }
                    }
                }

                // Throttled in-place subtitle update — never a list rebuild per chunk.
                if now.duration_since(speed.last_label_write) >= LABEL_UPDATE_INTERVAL {
                    speed.last_label_write = now;
                    let ema = speed.ema;
                    drop(speeds);
                    let bytes: u64 = widgets.track_bytes.borrow().get(item_id).map(|tracks| tracks.values().sum()).unwrap_or(0);
                    let batch = widgets.download_manager.batch_progress(&widgets.server_id, item_id);
                    if let Some(row) = widgets.live_rows.borrow().get(item_id.as_str()) {
                        row.set_subtitle(&downloading_subtitle(batch, bytes, Some(ema)));
                    }
                }
            }
        }
    });

    spawn_refresh(widgets);

    DownloadsScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks { status_page, list_box, scroller },
    }
}

fn spawn_refresh(widgets: Rc<Widgets>) {
    adw::glib::spawn_future_local(async move {
        let downloaded = abs_core::download_tracks::downloaded_item_ids(&widgets.pool, &widgets.server_id).await.unwrap_or_default();
        let mut ids: Vec<String> = downloaded.into_iter().collect();
        for id in widgets.downloading.borrow().iter() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let all_ids = ids.clone();

        // Rows are rebuilt from scratch every refresh; the in-flight rows re-register below.
        widgets.live_rows.borrow_mut().clear();

        while let Some(child) = widgets.list_box.row_at_index(0) {
            widgets.list_box.remove(&child);
        }

        let mut any_row = false;
        for item_id in ids {
            let Ok(item) = abs_storage::repo::items::get(&widgets.pool, &widgets.server_id, &item_id).await else { continue };
            let is_downloading = widgets.downloading.borrow().contains(&item_id);
            let tracks = abs_storage::repo::download_tracks::list_for_item(&widgets.pool, &widgets.server_id, &item_id).await.unwrap_or_default();

            // Seed the live byte map for this item: `bytes_downloaded` is the full size for
            // `Complete` tracks and the last checkpoint for pending/failed ones — exactly the
            // per-track cumulative value `TrackProgress` events will keep overwriting.
            widgets
                .track_bytes
                .borrow_mut()
                .insert(item_id.clone(), tracks.iter().map(|track| (track.ino.clone(), track.bytes_downloaded.max(0) as u64)).collect());

            let subtitle = if is_downloading {
                let batch = widgets.download_manager.batch_progress(&widgets.server_id, &item_id);
                let bytes: u64 = tracks.iter().map(|track| track.bytes_downloaded.max(0) as u64).sum();
                let speed = {
                    let speeds = widgets.speeds.borrow();
                    speeds
                        .get(&item_id)
                        .filter(|state| state.ema > 0.0 && state.last_instant.is_some_and(|at| at.elapsed() < SPEED_DISPLAY_WINDOW))
                        .map(|state| state.ema)
                };
                Some(downloading_subtitle(batch, bytes, speed))
            } else {
                let complete: Vec<_> = tracks.iter().filter(|track| track.status == DownloadStatus::Complete).collect();
                let size: u64 = complete.iter().map(|track| track.bytes_downloaded.max(0) as u64).sum();
                if complete.is_empty() { None } else { Some(format!("{}, {}", chapters_label(complete.len()), format_bytes(size))) }
            };

            let row = download_row(&item, is_downloading, &widgets.download_manager, &widgets.server_id, subtitle);
            if is_downloading {
                widgets.live_rows.borrow_mut().insert(item_id.clone(), row.clone());
            }
            widgets.list_box.append(&row);
            any_row = true;
        }

        // Drop bookkeeping for items that left the list entirely (cleared/removed downloads).
        widgets.track_bytes.borrow_mut().retain(|id, _| all_ids.contains(id));
        widgets.speeds.borrow_mut().retain(|id, _| all_ids.contains(id));

        widgets.status_page.set_visible(!any_row);
        widgets.scroller.set_visible(any_row);
    });
}

fn download_row(item: &abs_storage::models::Item, is_downloading: bool, download_manager: &DownloadManager, server_id: &str, subtitle: Option<String>) -> adw::ActionRow {
    const THUMBNAIL_SIZE: i32 = 48;
    let cover = crate::widgets::cover_image::CoverImage::new(THUMBNAIL_SIZE);
    cover.set_path(item.cover_cache_path.as_deref().map(std::path::Path::new));

    let row = adw::ActionRow::builder().title(&item.title).build();
    if let Some(text) = subtitle {
        row.set_subtitle(&text);
    }
    row.add_prefix(cover.widget());

    if is_downloading {
        let spinner = gtk4::Spinner::builder().spinning(true).valign(gtk4::Align::Center).build();
        // Stop, not cancel-delete: chapters that already completed stay downloaded (the row's own
        // subtitle switches to their "N chapters, size" summary once the batch winds down).
        let stop_button = gtk4::Button::builder().icon_name("process-stop-symbolic").tooltip_text("Stop").css_classes(["flat"]).valign(gtk4::Align::Center).build();
        stop_button.connect_clicked({
            let download_manager = download_manager.clone();
            let server_id = server_id.to_string();
            let item_id = item.id.clone();
            move |_| download_manager.cancel_item(&server_id, &item_id)
        });
        let box_ = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).build();
        box_.append(&spinner);
        box_.append(&stop_button);
        row.add_suffix(&box_);
    } else {
        let remove_button = gtk4::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Remove").css_classes(["flat"]).valign(gtk4::Align::Center).build();
        remove_button.connect_clicked({
            let download_manager = download_manager.clone();
            let server_id = server_id.to_string();
            let item_id = item.id.clone();
            move |_| download_manager.clear_item(&server_id, &item_id)
        });
        row.add_suffix(&remove_button);
    }

    row
}

/// "1 chapter" / "10 chapters" — the completed-download subtitle's first half.
fn chapters_label(count: usize) -> String {
    if count == 1 { "1 chapter".to_string() } else { format!("{count} chapters") }
}

/// "5 B", "18.2 MB", "1.3 GB" — decimal units, matching how servers report Content-Length.
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.1} kB", bytes as f64 / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// The in-flight subtitle: chapter progress from the manager's batch ("3/10 chapters"), total
/// bytes so far, and — while samples are flowing — the smoothed speed.
fn downloading_subtitle(batch: Option<(usize, usize)>, bytes: u64, speed: Option<f64>) -> String {
    let mut parts = Vec::new();
    if let Some((finished, total)) = batch {
        parts.push(format!("{finished}/{total} chapters"));
    }
    parts.push(format_bytes(bytes));
    if let Some(speed) = speed.filter(|rate| *rate > 0.0) {
        parts.push(format!("{}/s", format_bytes(speed as u64)));
    }
    parts.join(" · ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until, test_paths};
    use abs_storage::models::{Account, Server};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn session_for(pool: &SqlitePool, server_url: &str) -> (abs_core::auth::Session, Server, Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
        let server = abs_storage::repo::servers::get(pool, &server_id).await.unwrap();
        let account = abs_storage::repo::accounts::get(pool, &account_id).await.unwrap();
        (abs_core::auth::Session::new(pool.clone(), &server, &account), server, account)
    }

    async fn insert_synced_item(pool: &SqlitePool, server_id: &str, item_id: &str, title: &str) {
        abs_storage::repo::libraries::upsert(
            pool,
            abs_storage::repo::libraries::UpsertLibrary { id: "lib-1", server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 },
        )
        .await
        .unwrap();
        abs_storage::repo::items::upsert(
            pool,
            abs_storage::repo::items::UpsertItem {
                id: item_id,
                server_id,
                library_id: "lib-1",
                title,
                author: None,
                narrator: None,
                description: None,
                duration_seconds: 0.0,
                added_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    async fn mock_single_track_item(mock_server: &MockServer, item_id: &str, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": 5.0 }] }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_delay(delay).set_body_bytes(b"hello".to_vec()))
            .mount(mock_server)
            .await;
    }

    /// Two one-chapter-per-file tracks where the first completes immediately and the second is
    /// deliberately slow — the window a Stop press lands in after chapter 1 is already saved.
    async fn mock_two_track_item_slow_second(mock_server: &MockServer, item_id: &str, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [{ "ino": "1", "duration": 5.0 }, { "ino": "2", "duration": 5.0 }],
                    "chapters": [
                        { "id": 0, "start": 0.0, "end": 5.0, "title": "Chapter 1" },
                        { "id": 1, "start": 5.0, "end": 10.0, "title": "Chapter 2" },
                    ],
                }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/2")))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_delay(delay).set_body_bytes(b"hello".to_vec()))
            .mount(mock_server)
            .await;
    }

    fn test_download_manager(pool: SqlitePool) -> DownloadManager {
        DownloadManager::new(pool, test_paths(), Box::new(abs_player::network_watch::UnknownNetworkMonitor), false)
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`.
    pub(crate) fn run_empty_state_renders_when_nothing_is_downloaded(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool, test_paths(), server, account, session, manager);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(5));
        assert!(hooks.status_page.is_visible(), "empty state should render with nothing downloaded");
        assert!(!hooks.scroller.is_visible());
    }

    /// A download in progress shows a row with a cancel button, not a remove button; canceling it
    /// removes the row entirely (nothing ever completed for this item).
    pub(crate) fn run_in_progress_download_shows_a_cancel_row(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_single_track_item(&mock_server, "item-1", Duration::from_secs(2)));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool.clone(), test_paths(), server.clone(), account, session.clone(), manager.clone());
        let hooks = screen.test_hooks();

        manager.start_download(session, "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));
        assert!(hooks.scroller.is_visible(), "an in-flight download should show a row");

        manager.cancel_item(&server.id, "item-1");
        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(10));
        assert!(hooks.status_page.is_visible(), "canceling the only (never-completed) download should return to the empty state");
    }

    /// An in-flight download's row shows live progress (chapter fraction from the batch, bytes,
    /// speed while samples flow) in its subtitle, not just a spinner; once complete, the
    /// subtitle becomes the static "1 chapter, 5 B" summary.
    pub(crate) fn run_in_progress_download_shows_progress_and_speed(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_single_track_item(&mock_server, "item-1", Duration::from_secs(2)));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool.clone(), test_paths(), server.clone(), account, session.clone(), manager.clone());
        let hooks = screen.test_hooks();

        manager.start_download(session, "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        let row = hooks.list_box.row_at_index(0).unwrap().downcast::<adw::ActionRow>().unwrap();
        let subtitle = row.subtitle().unwrap();
        assert!(subtitle.contains("0/1 chapters"), "in-flight subtitle should show the batch's chapter progress, got: {subtitle}");
        assert!(subtitle.contains("B"), "in-flight subtitle should show bytes so far, got: {subtitle}");

        // Pure formatting, asserted alongside so the units stay pinned (no GTK involved).
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.2 MB");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(downloading_subtitle(Some((3, 10)), 18_900_000, Some(2_200_000.0)), "3/10 chapters · 18.9 MB · 2.2 MB/s");
        assert_eq!(chapters_label(1), "1 chapter");
        assert_eq!(chapters_label(10), "10 chapters");

        pump_until(
            || {
                hooks
                    .list_box
                    .row_at_index(0)
                    .and_then(|row| row.downcast::<adw::ActionRow>().ok())
                    .and_then(|row| row.subtitle())
                    .is_some_and(|subtitle| subtitle.contains("chapter") && subtitle.contains(","))
            },
            Duration::from_secs(10),
        );
        let subtitle = hooks.list_box.row_at_index(0).unwrap().downcast::<adw::ActionRow>().unwrap().subtitle().unwrap();
        assert_eq!(subtitle, "1 chapter, 5 B", "completed subtitle is the static chapters+size summary");
    }

    /// Pressing Stop mid-batch is a graceful end: the chapters that already completed stay
    /// downloaded (the row settles into their summary), the job winds down, and the state is
    /// `Stopped` — deliberately not a failure.
    pub(crate) fn run_stop_keeps_completed_chapters_and_stops_the_job(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item_slow_second(&mock_server, "item-1", Duration::from_secs(3)));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool.clone(), test_paths(), server.clone(), account, session.clone(), manager.clone());
        let hooks = screen.test_hooks();

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        manager.start_download(session, "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);

        // Wait until chapter 1's track is fully downloaded, then stop while chapter 2 lags.
        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().map(|row| row.status == DownloadStatus::Complete).unwrap_or(false),
            Duration::from_secs(10),
        );
        manager.cancel_item(&server.id, "item-1");

        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Stopped, .. })),
            Duration::from_secs(10),
        );

        // The completed chapter survived the stop…
        let track_1 = runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().unwrap();
        assert_eq!(track_1.status, DownloadStatus::Complete, "a stopped download keeps its completed chapters");
        // …and the row settles into the completed-summary view instead of disappearing.
        pump_until(
            || {
                hooks
                    .list_box
                    .row_at_index(0)
                    .and_then(|row| row.downcast::<adw::ActionRow>().ok())
                    .and_then(|row| row.subtitle())
                    .is_some_and(|subtitle| subtitle == "1 chapter, 5 B")
            },
            Duration::from_secs(10),
        );
    }

    /// A completed download shows a row with a remove button; removing it deletes the row and the
    /// underlying rows/files (verified indirectly via `downloaded_item_ids` going back to empty).
    pub(crate) fn run_completed_download_can_be_removed(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_single_track_item(&mock_server, "item-1", Duration::ZERO));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        manager.start_download(session.clone(), "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);
        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().map(|r| r.status == abs_storage::models::DownloadStatus::Complete).unwrap_or(false),
            Duration::from_secs(10),
        );

        let screen = build(pool.clone(), test_paths(), server.clone(), account, session, manager.clone());
        let hooks = screen.test_hooks();
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        manager.clear_item(&server.id, "item-1");
        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(10));
        assert!(hooks.status_page.is_visible(), "removing the only completed download should return to the empty state");
        assert!(runtime.block_on(abs_storage::repo::download_tracks::list_for_item(&pool, &server.id, "item-1")).unwrap().is_empty());
    }
}
