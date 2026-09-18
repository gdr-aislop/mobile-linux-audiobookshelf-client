//! Orchestrates the download pipeline's app-level concerns — queueing, concurrency limiting,
//! per-track cancellation, and event publishing — that `abs_core::download_tracks::download_track`
//! deliberately doesn't own (see that module's doc comment). Structured like
//! `player::PlayerController`: an `Rc<RefCell<Inner>>` driven via `glib::spawn_future_local`, with
//! the same dependency-injected shape (`abs_player::network_watch::NetworkMonitor`) so tests can
//! run without a real D-Bus/network stack.
//!
//! Never imports `abs_api` directly: every network call flows through `abs_core::streaming` (to
//! discover tracks/chapters the first time an item is downloaded without ever having been played)
//! and `abs_core::download_tracks` (the actual per-track fetch) — same boundary every other
//! screen/controller in this crate already respects.
//!
//! Deliberately does not persist or restore in-flight batches across an app restart: a download
//! killed mid-transfer leaves a resumable `download_tracks` row (per-track state lives in
//! `abs-storage`, not here), but resuming it automatically on the next launch is a product decision
//! for a later pass (the Downloads screen), not something this orchestrator does on its own.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use adw::glib;
use sqlx::SqlitePool;

use abs_core::auth::Session;
use abs_core::download_tracks::TrackDownloadOutcome;
use abs_core::downloads::DownloadScope;
use abs_player::network_watch::NetworkMonitor;
use abs_storage::models::DownloadStatus;
use abs_storage::AppPaths;

/// How many tracks may download concurrently across the whole app, regardless of how many items
/// are queued — bounded so downloads don't starve sync/cover/playback traffic sharing the same
/// connection. Not user-configurable (yet); a fixed, conservative default.
const MAX_CONCURRENT_DOWNLOADS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemDownloadState {
    /// Nothing in flight for this item — either it was never started, finished being cleared, or
    /// every in-flight track was canceled before the whole batch finished.
    Idle,
    Downloading,
    Complete,
    Failed,
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    TrackProgress { item_id: String, ino: String, bytes_downloaded: u64, total_bytes: Option<u64> },
    ItemStateChanged { item_id: String, state: ItemDownloadState },
}

type EventListener = Box<dyn Fn(&DownloadEvent)>;

/// Bookkeeping for one item's in-flight download batch: how many tracks were queued, how each one
/// finished so far, and every in-flight track's cooperative cancel flag so `cancel_item` can stop
/// them without tearing down the whole manager. Removed from `Inner::batches` once every track in
/// it has finished (successfully, failed, or canceled).
struct ItemBatch {
    total: usize,
    completed: usize,
    failed: usize,
    canceled: usize,
    cancel_flags: Vec<Rc<Cell<bool>>>,
}

impl ItemBatch {
    fn finished(&self) -> usize {
        self.completed + self.failed + self.canceled
    }
}

struct Inner {
    pool: SqlitePool,
    paths: AppPaths,
    network_monitor: Box<dyn NetworkMonitor>,
    wifi_only: bool,
    semaphore: Rc<tokio::sync::Semaphore>,
    listeners: Vec<EventListener>,
    /// Keyed by `(server_id, item_id)` rather than just `item_id` — the same item id is only ever
    /// meaningful within one server, but nothing stops two different servers from happening to
    /// reuse the same id, and this manager is shared for the app's whole lifetime, potentially
    /// across an account switch.
    batches: HashMap<(String, String), ItemBatch>,
}

impl Inner {
    fn publish(&self, event: DownloadEvent) {
        for listener in &self.listeners {
            listener(&event);
        }
    }
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Rc<RefCell<Inner>>,
}

impl DownloadManager {
    pub fn new(pool: SqlitePool, paths: AppPaths, network_monitor: Box<dyn NetworkMonitor>, wifi_only: bool) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                pool,
                paths,
                network_monitor,
                wifi_only,
                semaphore: Rc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
                listeners: Vec::new(),
                batches: HashMap::new(),
            })),
        }
    }

    /// Registers a permanent event listener, notified for the manager's whole lifetime — same
    /// "no corresponding unregister" shape as `PlayerController::add_listener` (nothing needs to
    /// stop listening once registered).
    pub fn add_listener(&self, listener: impl Fn(&DownloadEvent) + 'static) {
        self.inner.borrow_mut().listeners.push(Box::new(listener));
    }

    pub fn set_wifi_only(&self, value: bool) {
        self.inner.borrow_mut().wifi_only = value;
    }

    /// Resolves `scope` (relative to `current_chapter_index`) into the set of tracks it touches and
    /// starts fetching whichever of them aren't already complete. Fetches and caches track/chapter
    /// metadata first if it isn't already cached locally (e.g. this item has never been played), so
    /// downloading never requires having played the item first.
    pub fn start_download(&self, session: Session, item_id: String, scope: DownloadScope, current_chapter_index: usize) {
        let inner_rc = self.inner.clone();
        glib::spawn_future_local(async move {
            let pool = inner_rc.borrow().pool.clone();
            let server_id = session.server_id().to_string();

            let mut tracks = abs_core::tracks::cached_tracks(&pool, &server_id, &item_id).await.unwrap_or_default();
            let mut chapters = abs_core::chapters::cached_chapters(&pool, &server_id, &item_id).await.unwrap_or_default();

            if tracks.is_empty() {
                // Asked at resolve time, not captured earlier — same "never let a captured token
                // go stale across a long-running operation" posture as `PlayerController::start`.
                let access_token = session.access_token().await;
                match abs_core::streaming::resolve_stream_target(session.server_url(), &access_token, &item_id).await {
                    Ok(target) => {
                        if let Err(err) = abs_core::tracks::sync_item_tracks(&pool, &server_id, &item_id, &target.tracks).await {
                            tracing::warn!(%err, item_id, "couldn't persist tracks locally");
                        }
                        if let Err(err) = abs_core::chapters::sync_item_chapters(&pool, &server_id, &item_id, &target.chapters).await {
                            tracing::warn!(%err, item_id, "couldn't persist chapters locally");
                        }
                        tracks = abs_core::tracks::cached_tracks(&pool, &server_id, &item_id).await.unwrap_or_default();
                        chapters = abs_core::chapters::cached_chapters(&pool, &server_id, &item_id).await.unwrap_or_default();
                    }
                    Err(err) => {
                        tracing::warn!(%err, item_id, "couldn't resolve tracks for download");
                        inner_rc.borrow().publish(DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Failed });
                        return;
                    }
                }
            }

            if tracks.is_empty() {
                inner_rc.borrow().publish(DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Failed });
                return;
            }

            // An item with no chapter markers at all (rare, but real) has nothing for
            // `resolve_scope`/`tracks_needed_for_chapters` to work against — every scope
            // degenerates to "the whole book" in that case, since there's no finer-grained unit to
            // select from.
            let needed: BTreeSet<String> = if chapters.is_empty() {
                tracks.iter().map(|t| t.ino.clone()).collect()
            } else {
                let chapter_indices = abs_core::downloads::resolve_scope(scope, current_chapter_index, chapters.len());
                abs_core::download_tracks::tracks_needed_for_chapters(&tracks, &chapters, &chapter_indices)
            };

            // Filter out tracks already fully downloaded — `download_track` itself is idempotent
            // on a completed track, but filtering here keeps this batch's `total` (and therefore
            // its progress bookkeeping) accurate rather than counting cache hits as "in flight".
            let downloads = abs_storage::repo::download_tracks::list_for_item(&pool, &server_id, &item_id).await.unwrap_or_default();
            let complete: HashSet<&str> = downloads.iter().filter(|d| d.status == DownloadStatus::Complete).map(|d| d.ino.as_str()).collect();
            let pending_inos: Vec<String> = needed.into_iter().filter(|ino| !complete.contains(ino.as_str())).collect();

            if pending_inos.is_empty() {
                inner_rc.borrow().publish(DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Complete });
                return;
            }

            let cancel_flags: Vec<Rc<Cell<bool>>> = pending_inos.iter().map(|_| Rc::new(Cell::new(false))).collect();
            {
                let mut inner = inner_rc.borrow_mut();
                inner.batches.insert(
                    (server_id.clone(), item_id.clone()),
                    ItemBatch { total: pending_inos.len(), completed: 0, failed: 0, canceled: 0, cancel_flags: cancel_flags.clone() },
                );
                inner.publish(DownloadEvent::ItemStateChanged { item_id: item_id.clone(), state: ItemDownloadState::Downloading });
            }

            for (ino, cancel_flag) in pending_inos.into_iter().zip(cancel_flags) {
                Self::spawn_track_download(inner_rc.clone(), session.clone(), item_id.clone(), ino, cancel_flag);
            }
        });
    }

    /// Runs one track's download under the shared concurrency semaphore. `cancel_flag` is this
    /// track's own slot in its batch's `cancel_flags`, flipped by `cancel_item`.
    fn spawn_track_download(inner_rc: Rc<RefCell<Inner>>, session: Session, item_id: String, ino: String, cancel_flag: Rc<Cell<bool>>) {
        glib::spawn_future_local(async move {
            let semaphore = inner_rc.borrow().semaphore.clone();
            // A permit acquired before the metered check below matters: it's what actually bounds
            // how many attempts (including ones that immediately bail out for being on a metered
            // connection) can be "in flight" at once, keeping the accounting in `finish_track`
            // consistent regardless of why a track didn't proceed.
            let permit = semaphore.acquire().await.expect("semaphore is never closed");

            let (pool, paths, wifi_only, is_metered) = {
                let inner = inner_rc.borrow();
                (inner.pool.clone(), inner.paths.clone(), inner.wifi_only, inner.network_monitor.is_metered())
            };
            let server_id = session.server_id().to_string();

            // An unknown/undeterminable network type must never block a download — only a
            // *known* metered connection does, and only when the setting asks for it. This is
            // checked once per track start, not continuously (see this module's doc comment).
            if wifi_only && is_metered == Some(true) {
                drop(permit);
                Self::finish_track(&inner_rc, &server_id, &item_id, TrackDownloadOutcome::Failed("waiting for a non-metered connection".to_string()));
                return;
            }

            let cancel_check = {
                let flag = cancel_flag.clone();
                move || flag.get()
            };

            let progress_item_id = item_id.clone();
            let progress_ino = ino.clone();
            let progress_inner = inner_rc.clone();
            let on_progress = move |downloaded: u64, total: Option<u64>| {
                progress_inner.borrow().publish(DownloadEvent::TrackProgress {
                    item_id: progress_item_id.clone(),
                    ino: progress_ino.clone(),
                    bytes_downloaded: downloaded,
                    total_bytes: total,
                });
            };

            // Asked fresh for each track, not carried over from `start_download`'s own call — a
            // batch of many tracks (an "entire book" download) can easily outlast a short-lived
            // access token, and `Session::access_token` is exactly the "refresh if needed"
            // primitive `PlayerController` already relies on for the same reason.
            let access_token = session.access_token().await;
            let outcome = abs_core::download_tracks::download_track(&paths, &pool, session.server_url(), &access_token, &server_id, &item_id, &ino, on_progress, &cancel_check)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(%err, item_id = %item_id, ino = %ino, "download_track returned an error");
                    TrackDownloadOutcome::Failed(err.to_string())
                });

            drop(permit);
            Self::finish_track(&inner_rc, &server_id, &item_id, outcome);
        });
    }

    /// Records one track's outcome against its batch and, once every track in the batch has
    /// finished, publishes the item's overall state and removes the batch.
    fn finish_track(inner_rc: &Rc<RefCell<Inner>>, server_id: &str, item_id: &str, outcome: TrackDownloadOutcome) {
        let mut inner = inner_rc.borrow_mut();
        let key = (server_id.to_string(), item_id.to_string());
        let Some(batch) = inner.batches.get_mut(&key) else { return };

        match outcome {
            TrackDownloadOutcome::Completed => batch.completed += 1,
            TrackDownloadOutcome::Failed(_) => batch.failed += 1,
            TrackDownloadOutcome::Canceled => batch.canceled += 1,
        }

        if batch.finished() < batch.total {
            return;
        }

        let (failed, canceled, completed) = (batch.failed, batch.canceled, batch.completed);
        inner.batches.remove(&key);

        let state = if failed > 0 {
            ItemDownloadState::Failed
        } else if canceled > 0 && completed == 0 {
            // Every remaining track was canceled and none of them ever succeeded — a user-driven
            // stop, not a failure. A batch that's a mix of completed and canceled tracks (the user
            // canceled partway through) still counts as a failure to finish the *requested* scope,
            // surfaced as `Failed` so the caller knows the scope wasn't fully satisfied — a later
            // pass can query `abs_core::download_tracks::item_offline_availability` for the exact
            // partial picture.
            ItemDownloadState::Idle
        } else if canceled > 0 {
            ItemDownloadState::Failed
        } else {
            ItemDownloadState::Complete
        };
        inner.publish(DownloadEvent::ItemStateChanged { item_id: item_id.to_string(), state });
    }

    /// Flips the cancel flag for every track currently in flight for this item. Cooperative, not a
    /// hard abort — each track's own loop only checks it between chunks (see
    /// `abs_core::download_tracks::download_track`'s doc comment), so a cancellation always lands
    /// between well-defined units of work. Tracks that have already reached `Complete` are
    /// untouched, matching the ui-spec's "canceling leaves completed chapters in place."
    pub fn cancel_item(&self, server_id: &str, item_id: &str) {
        let inner = self.inner.borrow();
        if let Some(batch) = inner.batches.get(&(server_id.to_string(), item_id.to_string())) {
            for flag in &batch.cancel_flags {
                flag.set(true);
            }
        }
    }

    /// Cancels anything in flight for this item, then deletes every downloaded track file and row.
    /// Note: canceling is cooperative (see `cancel_item`), so a track's in-flight write and this
    /// deletion are not strictly ordered — a track that hasn't yet observed the cancellation when
    /// the delete runs could still write a few more bytes to a file this call is removing, or
    /// resurrect a row via its own next progress checkpoint. Accepted as a known race for this
    /// pass (clearing while actively downloading is an edge case, not the common path); a future
    /// pass could close it by having `clear_item` await the canceled tracks' own completion first.
    pub fn clear_item(&self, server_id: &str, item_id: &str) {
        self.cancel_item(server_id, item_id);
        let inner_rc = self.inner.clone();
        let server_id = server_id.to_string();
        let item_id = item_id.to_string();
        glib::spawn_future_local(async move {
            let pool = inner_rc.borrow().pool.clone();
            if let Err(err) = abs_core::download_tracks::clear_item_downloads(&pool, &server_id, &item_id).await {
                tracing::warn!(%err, item_id, "couldn't clear downloaded tracks");
                return;
            }
            inner_rc.borrow().publish(DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Idle });
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until, test_paths};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `abs_player::network_watch::FakeNetworkMonitor` is `#[cfg(test)]`-only *inside abs-player*,
    /// so it isn't compiled in when abs-player is built as an ordinary dependency of this crate's
    /// own test binary (same cross-crate `cfg(test)` visibility gap `main_window.rs`'s own tests
    /// document for `call_watch::FakeCallWatcher`) — a small local fake against the public
    /// `NetworkMonitor` trait sidesteps that instead.
    struct FakeNetworkMonitor {
        metered: Option<bool>,
    }

    impl NetworkMonitor for FakeNetworkMonitor {
        fn is_metered(&self) -> Option<bool> {
            self.metered
        }
    }

    async fn session_for(pool: &SqlitePool, server_url: &str) -> (Session, abs_storage::models::Server) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
        let server = abs_storage::repo::servers::get(pool, &server_id).await.unwrap();
        let account = abs_storage::repo::accounts::get(pool, &account_id).await.unwrap();
        (Session::new(pool.clone(), server_url, &server_id, &account), server)
    }

    async fn insert_synced_item(pool: &SqlitePool, server_id: &str, item_id: &str) {
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
                title: "Test Item",
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

    /// A two-file item, one chapter per file, so `DownloadScope::CurrentChapter` maps onto exactly
    /// one track — the simplest possible case that still exercises chapter->track resolution.
    async fn mock_two_track_item(mock_server: &MockServer, item_id: &str) {
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
        for ino in ["1", "2"] {
            Mock::given(method("GET"))
                .and(path(format!("/api/items/{item_id}/file/{ino}")))
                .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "audio/mpeg").insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
                .mount(mock_server)
                .await;
        }
    }

    /// Like `mock_two_track_item`, but with no chapter data at all — the degenerate case
    /// `start_download` falls back to "download every track" for, since there's no finer-grained
    /// scope to resolve against.
    async fn mock_two_track_item_without_chapters(mock_server: &MockServer, item_id: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": 5.0 }, { "ino": "2", "duration": 5.0 }] }
            })))
            .mount(mock_server)
            .await;
        for ino in ["1", "2"] {
            Mock::given(method("GET"))
                .and(path(format!("/api/items/{item_id}/file/{ino}")))
                .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "audio/mpeg").insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
                .mount(mock_server)
                .await;
        }
    }

    /// A single-track item whose file response is deliberately slow, giving `cancel_item` tests a
    /// real window to cancel inside before the transfer would otherwise complete.
    async fn mock_slow_single_track_item(mock_server: &MockServer, item_id: &str, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": 10.0 }] }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_delay(delay).set_body_bytes(b"hello".to_vec()))
            .mount(mock_server)
            .await;
    }

    pub(crate) fn run_start_download_fetches_only_the_needed_track_and_reaches_complete(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, "item-1"));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: None }), false);

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        manager.start_download(session, "item-1".to_string(), DownloadScope::CurrentChapter, 0);

        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Complete, .. })),
            Duration::from_secs(10),
        );
        assert!(
            events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Complete, .. })),
            "the download should have reached Complete"
        );

        // Only chapter 0's track ("1") should ever have been requested — chapter 1's track ("2")
        // is out of scope for `CurrentChapter` and must not be fetched.
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/1"));
        assert!(!requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/2"), "a track outside the requested scope must not be fetched");

        let row = runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Complete);
    }

    pub(crate) fn run_cancel_item_stops_the_track_from_reaching_complete(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_slow_single_track_item(&mock_server, "item-1", Duration::from_secs(2)));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: None }), false);

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        manager.start_download(session, "item-1".to_string(), DownloadScope::EntireBook, 0);

        // Give the manager a moment to actually issue the request before canceling — waiting for
        // `Downloading` (published synchronously once the batch is set up) rather than a fixed
        // sleep keeps this deterministic.
        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Downloading, .. })),
            Duration::from_secs(5),
        );
        manager.cancel_item(&server.id, "item-1");

        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Idle, .. })),
            Duration::from_secs(10),
        );
        assert!(
            events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Idle, .. })),
            "a fully-canceled batch with nothing completed should settle on Idle"
        );

        let row = runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap();
        assert!(row.is_none() || row.unwrap().status != DownloadStatus::Complete, "canceling must not let the track reach Complete");
    }

    pub(crate) fn run_wifi_only_blocks_a_download_on_a_metered_connection(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, "item-1"));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: Some(true) }), true);

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        manager.start_download(session, "item-1".to_string(), DownloadScope::CurrentChapter, 0);

        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Failed, .. })),
            Duration::from_secs(5),
        );
        assert!(
            events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Failed, .. })),
            "a metered connection with wifi_only set should fail the download rather than fetch it"
        );

        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(!requests.iter().any(|r| r.url.path().starts_with("/api/items/item-1/file/")), "a metered connection with wifi_only set must never fetch the file");
    }

    pub(crate) fn run_clear_item_deletes_files_and_publishes_idle(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, "item-1"));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: None }), false);
        manager.start_download(session, "item-1".to_string(), DownloadScope::CurrentChapter, 0);
        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().map(|r| r.status == DownloadStatus::Complete).unwrap_or(false),
            Duration::from_secs(10),
        );

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        manager.clear_item(&server.id, "item-1");

        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { state: ItemDownloadState::Idle, .. })),
            Duration::from_secs(10),
        );

        assert!(runtime.block_on(abs_storage::repo::download_tracks::list_for_item(&pool, &server.id, "item-1")).unwrap().is_empty());
    }

    /// An item with no chapter markers at all has nothing for `resolve_scope`/
    /// `tracks_needed_for_chapters` to select from — every scope must degenerate to "the whole
    /// book" rather than downloading nothing.
    pub(crate) fn run_start_download_with_no_chapters_fetches_every_track(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item_without_chapters(&mock_server, "item-1"));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: None }), false);
        manager.start_download(session, "item-1".to_string(), DownloadScope::CurrentChapter, 0);

        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::list_for_item(&pool, &server.id, "item-1")).unwrap().iter().filter(|d| d.status == DownloadStatus::Complete).count() == 2,
            Duration::from_secs(10),
        );

        let rows = runtime.block_on(abs_storage::repo::download_tracks::list_for_item(&pool, &server.id, "item-1")).unwrap();
        assert_eq!(rows.iter().filter(|d| d.status == DownloadStatus::Complete).count(), 2, "with no chapters, every track should be fetched regardless of scope");
    }

    /// Toggling `wifi_only` mid-batch only affects downloads started *after* the change — a track
    /// whose metered check already passed keeps running.
    pub(crate) fn run_set_wifi_only_only_affects_downloads_started_afterwards(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, "item-1"));
        runtime.block_on(mock_two_track_item(&mock_server, "item-2"));

        let pool = runtime.block_on(pool());
        let (session, server) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1"));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-2"));

        let manager = DownloadManager::new(pool.clone(), test_paths(), Box::new(FakeNetworkMonitor { metered: Some(true) }), false);

        let events: Rc<RefCell<Vec<DownloadEvent>>> = Rc::new(RefCell::new(Vec::new()));
        manager.add_listener({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });

        // Started while wifi_only is still false — a metered connection must not block it.
        manager.start_download(session.clone(), "item-1".to_string(), DownloadScope::CurrentChapter, 0);
        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Complete, .. } if item_id == "item-1"))
                || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Failed, .. } if item_id == "item-1")),
            Duration::from_secs(10),
        );
        assert!(
            events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Complete, .. } if item_id == "item-1")),
            "a download already in flight when wifi_only was false must not be blocked by a later toggle"
        );

        manager.set_wifi_only(true);

        // Started after the toggle, still on a metered connection — this one must be blocked.
        manager.start_download(session, "item-2".to_string(), DownloadScope::CurrentChapter, 0);
        pump_until(
            || events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Failed, .. } if item_id == "item-2")),
            Duration::from_secs(5),
        );
        assert!(
            events.borrow().iter().any(|e| matches!(e, DownloadEvent::ItemStateChanged { item_id, state: ItemDownloadState::Failed, .. } if item_id == "item-2")),
            "a download started after wifi_only was set true, on a metered connection, must be blocked"
        );
    }
}
