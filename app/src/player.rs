//! Bridges `abs-player`'s poll-based `AudioBackend` to GTK widgets: a mini-player bar (built here,
//! always present in `main_window` once something has played) and, optionally, a full player
//! screen's widgets (`screens::player`). This module — not `abs_core::playback::PlaybackState` —
//! is what actually drives real playback.
//!
//! `PlaybackState` is a simulated tick-based clock (`position += elapsed * speed`) with no method
//! to feed it an authoritative position from a real backend and no reference to `abs_player`
//! anywhere in `abs-core` (by design — `abs-core` has zero GStreamer dependency). Wiring a real
//! GStreamer pipeline's displayed position to that simulated clock would let the two drift apart,
//! which is a correctness bug, not an architecture shortcut — so this controller polls
//! `AudioBackend` directly instead, and `PlaybackState` stays unused. Don't "helpfully" reconnect
//! them without first giving `PlaybackState` a real way to reconcile with actual backend state.
//!
//! Never imports `abs_api`: the only network call this makes is
//! `abs_core::streaming::resolve_stream_target`, which builds its own authenticated
//! `abs_api::Client` internally — same boundary every other screen already respects.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::streaming::locate_track;
use abs_storage::AppPaths;

use crate::widgets::cover_image::CoverImage;

/// The URL/URI to actually load for a track: a local `file://` path when it's been verifiably
/// downloaded (see `abs_core::download_tracks::local_track_path` — never a stale/corrupt row), a
/// streaming URL otherwise. `gio::File::for_path(..).uri()` is the same correctly-percent-encoded
/// pattern this file already uses for MPRIS album-art URLs, not a raw `format!("file://{}", ..)`.
/// The access token is only fetched in the streaming branch — pointless (and, if genuinely
/// offline, noisy) to refresh a token for a track that's about to play from disk anyway.
async fn resolve_playable_url(
    pool: &SqlitePool,
    connection: &abs_core::connection::ConnectionTarget,
    server_id: &str,
    item_id: &str,
    ino: &str,
    session: &abs_core::auth::Session,
) -> String {
    if let Some(path) = abs_core::download_tracks::local_track_path(pool, server_id, item_id, ino).await {
        return gio::File::for_path(&path).uri().to_string();
    }
    connection.track_url(item_id, ino, &session.access_token().await)
}

/// The app is the composition root between `abs-core` (which resolves a server's connection
/// settings) and `abs-player` (which applies them to GStreamer) — this is the mapping between
/// the two, so neither crate depends on the other.
fn playback_properties(connection: &abs_core::connection::ConnectionTarget) -> abs_player::ConnectionProperties {
    abs_player::ConnectionProperties {
        extra_headers: connection.extra_headers().to_vec(),
        user_agent: connection.user_agent().map(str::to_string),
        ssl_strict: !connection.disable_ssl_verify(),
    }
}

const TICK_INTERVAL: Duration = Duration::from_millis(250);
const PROGRESS_WRITE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct PlayRequest {
    pub item_id: String,
    pub title: String,
    pub author: Option<String>,
}

#[derive(Clone)]
pub struct PlayerSnapshot {
    pub title: String,
    pub author: Option<String>,
    /// Book-level position — the current file's offset within the book plus the pipeline's
    /// position inside that file. Progress and chapters are defined book-level, so every
    /// consumer of a snapshot (scrubber, mini bar, MPRIS) speaks the same units.
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub is_playing: bool,
    pub speed: f64,
    pub sleep_timer_active: bool,
    pub cover_path: Option<std::path::PathBuf>,
}

/// A chapter, as needed by the chapters sheet — kept in-memory on `NowPlaying` rather than pushed
/// through `PlayerSnapshot` on every 250ms tick, since chapters don't change during playback.
#[derive(Clone)]
pub struct ChapterInfo {
    pub title: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

/// A sleep-timer deadline, checked once per `tick()` rather than driven by a second timer source
/// — a wall-clock deadline (15/30/45 min) and an end-of-chapter deadline (a stream position) are
/// otherwise different units entirely, but both reduce to "has `now` (real time, or playback
/// position) reached this yet?".
#[derive(Clone, Copy, PartialEq)]
enum SleepTimerDeadline {
    WallClock(Instant),
    Position(f64),
}

#[derive(Clone, Copy, PartialEq, Default)]
enum SleepTimerState {
    #[default]
    Off,
    Armed(SleepTimerDeadline),
}

struct NowPlaying {
    item_id: String,
    server_id: String,
    account_id: String,
    /// The live token source — asked per use (progress writes, track loads) rather than a token
    /// captured once, which dies within hours on servers v2.26.0+ while a book keeps playing.
    /// The connection (settings + resolved base URL) is asked through it at the same times.
    session: abs_core::auth::Session,
    title: String,
    author: Option<String>,
    /// Book-level duration (the sum of every track's duration), as `StreamTarget` reports it.
    duration_seconds: f64,
    /// One entry per audio file of the item, in book order — an item split across multiple files
    /// is played by advancing through these as each one ends.
    tracks: Vec<abs_core::streaming::StreamTrack>,
    /// Which entry of `tracks` the backend currently holds loaded.
    current_track: usize,
    is_playing: bool,
    chapters: Vec<ChapterInfo>,
    speed: f64,
    sleep_timer: SleepTimerState,
    cover_path: Option<std::path::PathBuf>,
}

type SnapshotListener = Box<dyn Fn(&PlayerSnapshot)>;

struct Inner {
    backend: Box<dyn abs_player::AudioBackend>,
    pool: SqlitePool,
    paths: AppPaths,
    now_playing: Option<NowPlaying>,
    /// Permanent listeners, notified on every published snapshot for the app's whole lifetime —
    /// the mini-player bar's closure is pushed here at construction, and MPRIS (once wired) is
    /// pushed here too via `PlayerController::add_listener`. Distinct from `full_update`, the one
    /// optional slot toggled as the full player screen opens/closes.
    listeners: Vec<SnapshotListener>,
    full_update: Option<SnapshotListener>,
    last_progress_write: Instant,
    /// Headphone unplug behavior, from `PlaybackSettings` (Settings → Playback) — see
    /// `handle_route_event`.
    pause_on_unplug: bool,
    resume_on_replug: bool,
    /// Live playback config, also from `PlaybackSettings` (Settings → Playback): the speed every
    /// `start()` begins a session at, and the skip intervals the transport buttons, keyboard
    /// actions and MPRIS next/previous use. Held here rather than captured by the various
    /// closures at build time, so a Settings edit takes effect immediately (see
    /// `set_playback_config`).
    default_speed: f64,
    skip_back_seconds: f64,
    skip_forward_seconds: f64,
    /// Whether the current pause was caused by `handle_route_event`'s unplug handling (as opposed
    /// to a manual pause, a phone call, a sleep timer or end-of-book). Only a pause this specific
    /// may ever be lifted by a replug. Cleared by every other pause path.
    paused_by_unplug: bool,
}

impl Inner {
    fn snapshot(&self) -> Option<PlayerSnapshot> {
        let now_playing = self.now_playing.as_ref()?;
        Some(PlayerSnapshot {
            title: now_playing.title.clone(),
            author: now_playing.author.clone(),
            position_seconds: self.book_position(),
            duration_seconds: now_playing.duration_seconds,
            is_playing: now_playing.is_playing,
            speed: now_playing.speed,
            sleep_timer_active: now_playing.sleep_timer != SleepTimerState::Off,
            cover_path: now_playing.cover_path.clone(),
        })
    }

    /// The book-level playback position: the current track's offset within the book plus the
    /// backend's position inside that track. Progress, chapters and bookmarks are all defined
    /// book-level (they're shared with the server and its other clients), while the pipeline only
    /// ever knows where it is inside the single file it holds — so every read of "where are we"
    /// funnels through here rather than through `backend.position()` directly.
    fn book_position(&self) -> f64 {
        let Some(now_playing) = &self.now_playing else { return 0.0 };
        let within_track = self.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        now_playing.tracks.get(now_playing.current_track).map(|t| t.offset_seconds).unwrap_or(0.0) + within_track
    }

    fn publish(&self) {
        let Some(snapshot) = self.snapshot() else { return };
        for listener in &self.listeners {
            listener(&snapshot);
        }
        if let Some(full_update) = &self.full_update {
            full_update(&snapshot);
        }
    }

    /// Fire-and-forget: spawns the actual DB write (and, best-effort, the server sync) rather
    /// than awaiting them, since every call site is a synchronous GTK signal handler or the tick
    /// timer, neither of which can await. Captures the position/ids up front rather than
    /// re-reading `self` from inside the spawned future. Reads the current position from the
    /// backend — for a write at an explicit position instead (marking finished, resetting),
    /// see `write_progress_at`.
    fn write_progress(&mut self, is_finished: bool) {
        let position = self.book_position();
        self.write_progress_at(position, is_finished);
    }

    /// The shared implementation behind `write_progress` (backend's current position),
    /// `PlayerController::mark_as_finished` (`duration_seconds`), and
    /// `PlayerController::reset_progress` (`0.0`) — same fire-and-forget local-write-then-sync
    /// shape in every case, differing only in which position gets written.
    fn write_progress_at(&mut self, position: f64, is_finished: bool) {
        let Some(now_playing) = &self.now_playing else { return };
        let pool = self.pool.clone();
        let account_id = now_playing.account_id.clone();
        let server_id = now_playing.server_id.clone();
        let item_id = now_playing.item_id.clone();
        let session = now_playing.session.clone();
        let duration_seconds = now_playing.duration_seconds;
        self.last_progress_write = Instant::now();

        glib::spawn_future_local(async move {
            if let Err(err) = abs_storage::repo::progress::set(&pool, &account_id, &server_id, &item_id, position, is_finished).await
            {
                tracing::warn!(%err, "couldn't persist playback progress");
            }
            // Best-effort: the local write above is this client's own source of truth (Home's
            // "Continue Listening" reads it), so a network hiccup syncing it up to the server
            // must not be treated as a playback error. The token — and the connection — are
            // asked at write time; these writes happen for as long as the app is open, well
            // past any single token's life or any settings change.
            let access_token = session.access_token().await;
            let connection = match session.connection_target().await {
                Ok(connection) => connection,
                Err(err) => {
                    tracing::warn!(%err, "couldn't load the server's connection settings; progress stays local");
                    return;
                }
            };
            if let Err(err) =
                abs_core::streaming::sync_progress_to_server(&connection, &access_token, &item_id, position, duration_seconds, is_finished)
                    .await
            {
                tracing::warn!(%err, "couldn't sync playback progress to the server");
            }
        });
    }

    /// At end-of-stream: if another track follows the current one, refines the track map against
    /// the file that just finished and returns `(item_id, next_index)` for
    /// `spawn_load_track` — leaving `is_playing` set, since from the state machine's point of
    /// view playback continues. `None` means the item really is over and the caller should run
    /// its existing pause-and-mark-finished path.
    fn next_track_after_end_of_stream(&mut self) -> Option<(String, usize)> {
        let now_playing = self.now_playing.as_mut()?;
        let next = now_playing.current_track + 1;
        if next >= now_playing.tracks.len() {
            return None;
        }

        // Prefer the pipeline's *actual* duration for the file that just ended over the
        // server-reported one (which can be missing — parsed as 0.0 — or slightly off): shift
        // the following tracks' offsets by the difference, so book-level positions stay
        // continuous across the boundary and don't jump backwards when a duration was unknown.
        if let Some(actual) = self.backend.duration().filter(|d| !d.is_zero()) {
            let delta =
                now_playing.tracks[now_playing.current_track].offset_seconds + actual.as_secs_f64()
                    - now_playing.tracks[next].offset_seconds;
            if delta.abs() > f64::EPSILON {
                for track in &mut now_playing.tracks[next..] {
                    track.offset_seconds += delta;
                }
                // The book-level total is exactly the last track's offset plus its own duration
                // — shifting every later offset by `delta` shifts that sum by `delta` too, so the
                // total has to move with it. Left stale, it would silently drift from the
                // corrected timeline on every mismatch, throwing off `mark_as_finished`'s
                // recorded position, `seek_to_seconds`/`skip`'s clamp bound, and the book-level
                // duration shown in the scrubber and reported to MPRIS.
                now_playing.duration_seconds += delta;
            }
        }

        now_playing.current_track = next;
        Some((now_playing.item_id.clone(), next))
    }

    /// Loads item `track_index`'s file and optionally seeks `within_seconds` into it, on the GLib
    /// main loop rather than synchronously — both callers (end-of-track advance and cross-track
    /// seeks) run from the tick handler, which must not block on a network-streamed pipeline's
    /// preroll. Seeking — including `set_speed`, which is itself a seek-with-rate — needs the
    /// pipeline to have actually *reached* `PAUSED` (the same readiness requirement and polling
    /// pattern as `start()`'s resume seek), so anything needing a seek waits for `position()` to
    /// become available first; a plain start-of-track load skips the wait entirely. `load()`
    /// resets the pipeline's speed to 1.0, so the item's speed is re-applied afterwards.
    /// Resumes playing only if the item is still flagged as playing at that point, so a user
    /// pausing mid-transition is respected rather than overridden.
    fn spawn_load_track(inner_rc: Rc<RefCell<Inner>>, item_id: String, track_index: usize, within_seconds: f64) {
        glib::spawn_future_local(async move {
            let (pool, server_id, session, ino, speed) = {
                let inner = inner_rc.borrow();
                let Some(now_playing) = &inner.now_playing else { return };
                if now_playing.item_id != item_id {
                    return;
                }
                let Some(track) = now_playing.tracks.get(track_index) else { return };
                (inner.pool.clone(), now_playing.server_id.clone(), now_playing.session.clone(), track.ino.clone(), now_playing.speed)
            };
            // The connection is asked at load time — a mid-book settings change (local address,
            // headers, TLS) is honored by the next track. Failure means the server row is gone
            // (session removed underneath us); stop cleanly like a load failure.
            let connection = match session.connection_target().await {
                Ok(connection) => connection,
                Err(err) => {
                    tracing::warn!(%err, track = track_index, "couldn't load the server's connection settings");
                    let mut inner = inner_rc.borrow_mut();
                    if let Some(now_playing) = &mut inner.now_playing {
                        if now_playing.item_id == item_id {
                            now_playing.is_playing = false;
                        }
                    }
                    inner.publish();
                    return;
                }
            };
            // Prefers a verifiably-downloaded local file over streaming; the streaming URL, when
            // used, is rebuilt with a current token rather than reusing whatever `resolve_stream_target`
            // baked in at resolve time — by the time a multi-file book advances (possibly hours
            // later) that one can be expired.
            let url = resolve_playable_url(&pool, &connection, &server_id, &item_id, &ino, &session).await;

            {
                let mut inner = inner_rc.borrow_mut();
                // Transport properties must be applied before the load: GStreamer creates the
                // HTTP source during it, and source-setup reads what was last applied here.
                inner.backend.apply_connection(&playback_properties(&connection));
                if let Err(err) = inner.backend.load(&url) {
                    tracing::warn!(%err, track = track_index, "couldn't load the next track");
                    if let Some(now_playing) = &mut inner.now_playing {
                        if now_playing.item_id == item_id {
                            now_playing.is_playing = false;
                        }
                    }
                    inner.publish();
                    return;
                }
            }

            let needs_seek_readiness = within_seconds > 0.0 || (speed - 1.0).abs() > f64::EPSILON;
            if needs_seek_readiness {
                let _ = inner_rc.borrow_mut().backend.pause();
                for _ in 0..50 {
                    if inner_rc.borrow().backend.position().is_some() {
                        break;
                    }
                    glib::timeout_future(Duration::from_millis(100)).await;
                }
                if within_seconds > 0.0 {
                    let _ = inner_rc.borrow_mut().backend.seek(Duration::from_secs_f64(within_seconds));
                }
                if (speed - 1.0).abs() > f64::EPSILON {
                    let _ = inner_rc.borrow_mut().backend.set_speed(speed);
                }
            }

            let mut inner = inner_rc.borrow_mut();
            if inner.now_playing.as_ref().is_some_and(|np| np.item_id == item_id && np.is_playing) {
                let _ = inner.backend.play();
            }
            inner.publish();
        });
    }
}

#[derive(Clone)]
pub struct PlayerController {
    inner: Rc<RefCell<Inner>>,
    /// The tick timer runs for as long as the controller exists — fine in production (one
    /// controller for the whole app's lifetime), but tests build a fresh controller per scenario
    /// and, without an explicit way to stop it, every one of those timers keeps firing forever on
    /// the shared GLib main context, interleaving with (and slowing down) whatever scenario runs
    /// next. `stop()` removes it; call it at the end of any test that builds a controller.
    tick_source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl PlayerController {
    pub fn new(
        pool: SqlitePool,
        paths: AppPaths,
        backend: Box<dyn abs_player::AudioBackend>,
        mini_update: impl Fn(&PlayerSnapshot) + 'static,
    ) -> Self {
            let controller = Self {
                inner: Rc::new(RefCell::new(Inner {
                    backend,
                    pool,
                    paths,
                    now_playing: None,
                    listeners: vec![Box::new(mini_update)],
                    full_update: None,
                    last_progress_write: Instant::now(),
                    // Overridden right after construction via `set_headphone_behavior` and
                    // `set_playback_config` (the settings aren't known to `new()`'s signature)
                    // — false/false is the safe "do nothing automatically" middle, and the
                    // playback defaults below are `PlaybackSettings`' own defaults.
                    pause_on_unplug: false,
                    resume_on_replug: false,
                    default_speed: abs_core::playback::DEFAULT_SPEED,
                    skip_back_seconds: 15.0,
                    skip_forward_seconds: 30.0,
                    paused_by_unplug: false,
                })),
                tick_source: Rc::new(RefCell::new(None)),
            };

        let source_id = glib::timeout_add_local(TICK_INTERVAL, {
            let controller = controller.clone();
            move || {
                controller.tick();
                glib::ControlFlow::Continue
            }
        });
        *controller.tick_source.borrow_mut() = Some(source_id);

        controller
    }

    /// Stops the tick timer permanently. See the `tick_source` field doc — production never needs
    /// this (one controller for the app's lifetime); tests should call it once done with a
    /// controller so its timer doesn't keep running into later scenarios.
    #[cfg(test)]
    pub fn stop(&self) {
        if let Some(id) = self.tick_source.borrow_mut().take() {
            id.remove();
        }
    }

    pub fn set_full_update(&self, update: impl Fn(&PlayerSnapshot) + 'static) {
        self.inner.borrow_mut().full_update = Some(Box::new(update));
    }

    pub fn clear_full_update(&self) {
        self.inner.borrow_mut().full_update = None;
    }

    /// Registers a permanent snapshot listener, notified alongside the mini-player bar's for the
    /// app's whole lifetime — unlike `set_full_update`, this has no corresponding "clear" (nothing
    /// needs to stop listening once registered; MPRIS is the first user of this).
    pub fn add_listener(&self, listener: impl Fn(&PlayerSnapshot) + 'static) {
        self.inner.borrow_mut().listeners.push(Box::new(listener));
    }

    pub fn snapshot(&self) -> Option<PlayerSnapshot> {
        self.inner.borrow().snapshot()
    }

    /// The currently-playing item's chapters, if any — for the chapters sheet. Empty if nothing
    /// is playing or the item has no chapter data.
    pub fn chapters(&self) -> Vec<ChapterInfo> {
        self.inner.borrow().now_playing.as_ref().map(|np| np.chapters.clone()).unwrap_or_default()
    }

    /// `(session, server_id, item_id)` for whatever is currently loaded — the context a download
    /// button needs to call `DownloadManager::start_download`. `None` if nothing is playing.
    pub fn current_download_context(&self) -> Option<(abs_core::auth::Session, String, String)> {
        let inner = self.inner.borrow();
        let now_playing = inner.now_playing.as_ref()?;
        Some((now_playing.session.clone(), now_playing.server_id.clone(), now_playing.item_id.clone()))
    }

    /// Index into `chapters()` that the current book-level position falls in — the same
    /// `start_seconds <= position && position < end_seconds` test `build_chapter_row` uses to
    /// highlight "the current chapter", pulled out here so a download button can default its
    /// scope to it too. `None` if nothing is playing or the item has no chapter data; a position
    /// past every chapter's range (a rare rounding edge) clamps to the last chapter.
    pub fn current_chapter_index(&self) -> Option<usize> {
        let inner = self.inner.borrow();
        let now_playing = inner.now_playing.as_ref()?;
        if now_playing.chapters.is_empty() {
            return None;
        }
        let position = inner.book_position();
        now_playing
            .chapters
            .iter()
            .position(|c| c.start_seconds <= position && position < c.end_seconds)
            .or(Some(now_playing.chapters.len() - 1))
    }

    /// Records a bookmark at the current position. Local-only, bypassing `abs-core` entirely —
    /// same precedent as `write_progress`'s local half: a plain repo write, no server sync, since
    /// none is specified for bookmarks. A no-op if nothing is playing.
    pub fn add_bookmark(&self) {
        let inner = self.inner.borrow();
        let Some(now_playing) = &inner.now_playing else { return };
        let pool = inner.pool.clone();
        let account_id = now_playing.account_id.clone();
        let server_id = now_playing.server_id.clone();
        let item_id = now_playing.item_id.clone();
        let position = inner.book_position();
        drop(inner);

        glib::spawn_future_local(async move {
            if let Err(err) = abs_storage::repo::bookmarks::add(&pool, &account_id, &server_id, &item_id, position).await {
                tracing::warn!(%err, "couldn't save bookmark");
            }
        });
    }

    /// Pauses (if playing) and marks the current item finished at its full duration — the same
    /// end state reaching the actual end of the book leaves it in, triggered manually. Useful for
    /// testing (no more manually scrubbing to the end or editing the DB by hand) and a real,
    /// shippable action in its own right — Audiobookshelf's other clients let you do the same. A
    /// no-op if nothing is playing.
    pub fn mark_as_finished(&self) {
        let mut inner = self.inner.borrow_mut();
        let Some(duration_seconds) = inner.now_playing.as_ref().map(|np| np.duration_seconds) else { return };
        if inner.backend.pause().is_ok() {
            if let Some(now_playing) = &mut inner.now_playing {
                now_playing.is_playing = false;
            }
        }
        inner.publish();
        inner.write_progress_at(duration_seconds, true);
    }

    /// Resets the current item's progress back to the start — local and server — and seeks
    /// playback there too, so the effect is visible immediately without reopening anything.
    /// Useful for testing (repeatedly restarting a book from scratch) and, per the same reasoning
    /// as `mark_as_finished`, worth keeping as real functionality rather than a debug-only
    /// backdoor. A no-op if nothing is playing.
    pub fn reset_progress(&self) {
        if self.inner.borrow().now_playing.is_none() {
            return;
        }
        // Book position 0 is track 0's start — which is a cross-track seek whenever a later file
        // is loaded, so this goes through `seek_to_seconds`'s mapping rather than the backend
        // directly.
        self.seek_to_seconds(0.0);
        let mut inner = self.inner.borrow_mut();
        inner.publish();
        inner.write_progress_at(0.0, false);
    }

    /// Resolves a playable URL and starts playback, resuming from any existing progress for this
    /// item/account rather than always starting over from position 0. `default_speed` is
    /// `PlaybackSettings::default_speed`, applied once at the start of every session (the user can
    /// change it afterwards via `set_speed`).
    pub fn start(&self, session: abs_core::auth::Session, item: PlayRequest, default_speed: f64) {
        let inner_rc = self.inner.clone();
        glib::spawn_future_local(async move {
            let (pool, paths) = {
                let inner = inner_rc.borrow();
                (inner.pool.clone(), inner.paths.clone())
            };

            // Asked at resolve time, not captured earlier — see `NowPlaying::session`. The
            // connection (settings + resolved base URL) is asked the same way: a settings
            // change is honored by the very next playback without any rebuild.
            let access_token = session.access_token().await;
            let connection = match session.connection_target().await {
                Ok(connection) => connection,
                Err(err) => {
                    tracing::warn!(%err, item_id = %item.item_id, "couldn't load the server's connection settings");
                    return;
                }
            };

            // The cover fetch is detached from playback start entirely: even bounded by its
            // own timeout, waiting for it in the join below could delay the first note on a
            // slow network (each cover fetch opens a fresh connection — cold DNS + TCP + TLS
            // before any bytes move). Instead it runs as its own background task and lands in
            // the snapshot whenever it's ready — cache hits within a tick, cold fetches after —
            // which the mini-player, player screen and MPRIS art all pick up automatically.
            {
                let inner_rc = inner_rc.clone();
                let pool = pool.clone();
                let paths = paths.clone();
                let connection = connection.clone();
                let access_token = access_token.clone();
                let server_id = session.server_id().to_string();
                let item_id = item.item_id.clone();
                // Runs on the GTK main loop (`spawn_future_local`, not `tokio::spawn`): it holds
                // `Rc`s and borrows main-loop state, and the reqwest-driven fetch works because
                // main.rs keeps the Tokio runtime entered for the GTK loop's lifetime — the same
                // shape as every other future in this file.
                glib::spawn_future_local(async move {
                    let Some(cover_path) =
                        abs_core::covers::fetch_and_cache_cover(&paths, &pool, &connection, &access_token, &server_id, &item_id).await
                    else {
                        return;
                    };
                    let mut inner = inner_rc.borrow_mut();
                    match &mut inner.now_playing {
                        Some(now_playing) if now_playing.item_id == item_id => now_playing.cover_path = Some(cover_path),
                        // Playback ended or switched items while the fetch was in flight.
                        _ => return,
                    }
                    inner.publish();
                });
            }

            // Resolving the stream URL is required to proceed — unless the item can be played
            // from locally cached state instead (below). Reconciling progress is a nice-to-have
            // that must never add its own delay on top — run both concurrently rather than one
            // after another, so a slow or unreachable server is only ever felt once, not twice.
            let (target_result, reconcile_result) = tokio::join!(
                abs_core::streaming::resolve_stream_target(&connection, &access_token, &item.item_id),
                abs_core::progress_sync::reconcile_item_progress(&pool, &connection, &access_token, session.account_id(), session.server_id(), &item.item_id),
            );
            if let Err(err) = reconcile_result {
                tracing::warn!(%err, item_id = %item.item_id, "couldn't reconcile progress with the server; using local progress");
            }
            let target = match target_result {
                Ok(target) => target,
                Err(err) => {
                    // The server is unreachable (or errored): fall back to locally cached track
                    // metadata, letting downloaded files play offline. A fully-downloaded item
                    // plays end-to-end; a partially-downloaded one starts and stops cleanly at
                    // the first gap (`spawn_load_track`'s load failure path); an item never
                    // resolved on this device has nothing to fall back on and can't start.
                    match abs_core::streaming::offline_stream_target(&pool, session.server_id(), &item.item_id, &connection, &access_token).await {
                        Ok(offline) => {
                            tracing::info!(item_id = %item.item_id, "couldn't reach the server; playing from locally cached tracks");
                            offline
                        }
                        Err(offline_err) => {
                            tracing::warn!(%err, offline = %offline_err, item_id = %item.item_id, "couldn't resolve a playable URL");
                            return;
                        }
                    }
                }
            };

            // Reads whatever `reconcile_item_progress` just wrote, if it succeeded — falling back
            // to this client's own last local write (or nothing) otherwise. Book-level, so it can
            // fall in any track of a multi-file item.
            let resume_at = abs_storage::repo::progress::get(&pool, session.account_id(), session.server_id(), &item.item_id)
                .await
                .ok()
                .flatten()
                .map(|p| p.current_time_seconds)
                .filter(|s| *s > 0.0 && *s < target.duration_seconds);

            // A book-level resume position maps to (track, within-track) — the right file is the
            // one that gets loaded, rather than always starting from the first and trusting the
            // position to be inside it.
            let (start_track, start_within) =
                resume_at.map(|at| locate_track(&target.tracks, at)).unwrap_or((0, 0.0));

            // Local DB write only, no network involved — inline rather than part of the
            // `tokio::join!` above, which is reserved for concurrent network calls. (Deliberately
            // NOT syncing tracks here: `upsert_all` deletes and re-inserts the item's track rows,
            // and `download_tracks` cascades on that FK, so a playback-time sync would wipe
            // existing download state. The download manager syncs tracks itself, before any
            // download of an item that was never synced before.)
            if let Err(err) = abs_core::chapters::sync_item_chapters(&pool, session.server_id(), &item.item_id, &target.chapters).await {
                tracing::warn!(%err, item_id = %item.item_id, "couldn't persist chapters locally");
            }
            let chapters = target
                .chapters
                .iter()
                .map(|c| ChapterInfo { title: c.title.clone(), start_seconds: c.start_seconds, end_seconds: c.end_seconds })
                .collect();

            let start_url = resolve_playable_url(&pool, &connection, session.server_id(), &item.item_id, &target.tracks[start_track].ino, &session).await;

            {
                let mut inner = inner_rc.borrow_mut();
                // As in `spawn_load_track`: transport properties go on before the load, so the
                // HTTP source created during it is set up with the connection's settings.
                inner.backend.apply_connection(&playback_properties(&connection));
                if let Err(err) = inner.backend.load(&start_url) {
                    tracing::warn!(%err, "couldn't load the audio stream");
                    return;
                }
                // A seek needs the pipeline to have actually *reached* PAUSED, not just been
                // asked to — pausing is itself an async state change, and for a network-streamed
                // source (connecting, buffering) it can take a real moment. Requesting the seek
                // before that lands is not an error `AudioBackend` reports; it silently no-ops,
                // which used to mean "resume mid-book" quietly resumed from 0 instead.
                let _ = inner.backend.pause();
            }
            // A non-default speed also needs a seek internally (`AudioBackend::set_speed` is
            // implemented as a seek-with-rate, GStreamer having no rate-only call), so it has the
            // same readiness requirement as the resume seek below.
            let needs_seek_ready = start_within > 0.0 || (default_speed - 1.0).abs() > f64::EPSILON;
            if needs_seek_ready {
                // `duration()` can come back `Some` from container metadata alone, before the
                // pipeline has actually finished prerolling into `PAUSED` — which is what seeking
                // actually requires. `position()` only starts returning a value once preroll has
                // genuinely completed, so it's the more accurate "ready to seek" signal.
                for _ in 0..50 {
                    if inner_rc.borrow().backend.position().is_some() {
                        break;
                    }
                    glib::timeout_future(Duration::from_millis(100)).await;
                }
            }

            let mut inner = inner_rc.borrow_mut();
            if start_within > 0.0 {
                let _ = inner.backend.seek(Duration::from_secs_f64(start_within));
            }
            // `set_speed` is itself a seek-with-rate (see `abs-player`'s own doc comment) — even
            // setting it to the already-default 1.0 would perform a redundant seek that queries
            // `position()` and re-seeks to it, which can race the resume seek just above (if the
            // resume seek's position update hasn't propagated yet, this would re-seek back to the
            // stale pre-resume position). Skip it entirely when there's nothing to change.
            let applied_speed = if (default_speed - 1.0).abs() > f64::EPSILON {
                if inner.backend.set_speed(default_speed).is_ok() { default_speed } else { 1.0 }
            } else {
                1.0
            };
            let _ = inner.backend.play();

            inner.now_playing = Some(NowPlaying {
                item_id: item.item_id,
                server_id: session.server_id().to_string(),
                account_id: session.account_id().to_string(),
                session,
                title: item.title,
                author: item.author,
                duration_seconds: target.duration_seconds,
                tracks: target.tracks,
                current_track: start_track,
                is_playing: true,
                chapters,
                speed: applied_speed,
                sleep_timer: SleepTimerState::Off,
                // Filled in by the detached cover-fetch task above once it lands (cache hits
                // within a tick, cold fetches when the fetch finishes) — never gating start.
                cover_path: None,
            });
            inner.last_progress_write = Instant::now();
            inner.publish();
        });
    }

    pub fn play(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.backend.play().is_ok() {
            if let Some(now_playing) = &mut inner.now_playing {
                now_playing.is_playing = true;
            }
        }
        inner.publish();
    }

    pub fn pause(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.paused_by_unplug = false;
        if inner.backend.pause().is_ok() {
            if let Some(now_playing) = &mut inner.now_playing {
                now_playing.is_playing = false;
            }
        }
        inner.publish();
        inner.write_progress(false);
    }

    /// Applies Settings → Playback's headphone behavior switches (persisted; the live controller
    /// must follow immediately, not on the next app start).
    pub fn set_headphone_behavior(&self, pause_on_unplug: bool, resume_on_replug: bool) {
        let mut inner = self.inner.borrow_mut();
        inner.pause_on_unplug = pause_on_unplug;
        inner.resume_on_replug = resume_on_replug;
    }

    /// What `set_headphone_behavior` last applied — for asserting that the Settings screen's
    /// switches actually reach the controller.
    #[cfg(test)]
    pub fn headphone_behavior(&self) -> (bool, bool) {
        let inner = self.inner.borrow();
        (inner.pause_on_unplug, inner.resume_on_replug)
    }

    /// Applies Settings → Playback's start-of-session speed and skip intervals (persisted; the
    /// live controller must follow immediately, not on the next app start). Same story as
    /// `set_headphone_behavior` — the consumers that used to capture these values at build time
    /// (the play closure in `main_window`, the player screen's skip buttons and actions,
    /// `MprisBridge`) read the getters below at call time instead.
    pub fn set_playback_config(&self, default_speed: f64, skip_back_seconds: f64, skip_forward_seconds: f64) {
        let mut inner = self.inner.borrow_mut();
        inner.default_speed = default_speed;
        inner.skip_back_seconds = skip_back_seconds;
        inner.skip_forward_seconds = skip_forward_seconds;
    }

    /// The speed every `start()` begins a session at — Settings → Playback's "Default speed".
    pub fn default_speed(&self) -> f64 {
        self.inner.borrow().default_speed
    }

    /// The skip intervals as `(back, forward)` seconds — Settings → Playback's skip rows.
    pub fn skip_intervals(&self) -> (f64, f64) {
        let inner = self.inner.borrow();
        (inner.skip_back_seconds, inner.skip_forward_seconds)
    }

    /// Reacts to `abs_player::route_watch` events: an unplug pauses (when enabled, and only if
    /// something is actually playing — that is the pause a replug may later lift), a replug
    /// resumes **only** that kind of pause, and only when enabled. A manual pause, phone call,
    /// sleep timer or end-of-book pause clears the "paused by unplug" mark (in `pause()` and
    /// `tick()`'s own pause paths), so none of them can ever be overridden by a reconnection.
    pub fn handle_route_event(&self, event: abs_player::route_watch::RouteEvent) {
        match event {
            abs_player::route_watch::RouteEvent::Unplugged => {
                let was_playing = {
                    let inner = self.inner.borrow();
                    inner.pause_on_unplug && inner.now_playing.as_ref().is_some_and(|np| np.is_playing)
                };
                if !was_playing {
                    return;
                }
                // `pause()` clears the mark (it's the generic pause path — also used for calls,
                // MPRIS and the user); the unplug then re-marks it as *its* pause.
                self.pause();
                self.inner.borrow_mut().paused_by_unplug = true;
            }
            abs_player::route_watch::RouteEvent::Replugged => {
                let should_resume = {
                    let inner = self.inner.borrow();
                    inner.resume_on_replug
                        && inner.paused_by_unplug
                        && inner.now_playing.as_ref().is_some_and(|np| !np.is_playing)
                };
                if should_resume {
                    self.inner.borrow_mut().paused_by_unplug = false;
                    self.play();
                }
            }
        }
    }

    pub fn toggle_play_pause(&self) {
        let is_playing = self.snapshot().map(|s| s.is_playing).unwrap_or(false);
        if is_playing {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn skip(&self, delta_seconds: f64) {
        let target = {
            let inner = self.inner.borrow();
            let Some(now_playing) = &inner.now_playing else { return };
            let current = inner.book_position();
            (current + delta_seconds).clamp(0.0, now_playing.duration_seconds)
        };
        self.seek_to_seconds(target);
    }

    pub fn seek_fraction(&self, fraction: f64) {
        let Some(duration_seconds) = self.inner.borrow().now_playing.as_ref().map(|np| np.duration_seconds) else { return };
        self.seek_to_seconds(fraction.clamp(0.0, 1.0) * duration_seconds);
    }

    /// Seeks to an absolute book-level position — the primitive behind `seek_fraction`, `skip`,
    /// the chapters sheet (tap-to-seek to a chapter's start) and MPRIS `Seek`/`SetPosition`. A
    /// position inside a different file than the one loaded (or past its end) is a cross-track
    /// seek: the state machine switches to the target track and `spawn_load_track` brings the
    /// actual pipeline there asynchronously.
    pub fn seek_to_seconds(&self, seconds: f64) {
        let cross_track = {
            let mut inner = self.inner.borrow_mut();
            let Some(now_playing) = &inner.now_playing else { return };
            let target = seconds.clamp(0.0, now_playing.duration_seconds);
            let (track_index, within) = locate_track(&now_playing.tracks, target);
            if track_index == now_playing.current_track {
                let _ = inner.backend.seek(Duration::from_secs_f64(within));
                inner.publish();
                None
            } else {
                let now_playing = inner.now_playing.as_mut().expect("checked just above");
                now_playing.current_track = track_index;
                let item_id = now_playing.item_id.clone();
                inner.publish();
                Some((item_id, track_index, within))
            }
        };
        if let Some((item_id, track_index, within)) = cross_track {
            Inner::spawn_load_track(self.inner.clone(), item_id, track_index, within);
        }
    }

    /// Changes the playback rate. Only updates the reported speed if the backend actually
    /// accepted it, matching `play()`/`pause()`'s existing "state reflects reality" pattern.
    pub fn set_speed(&self, speed: f64) {
        let mut inner = self.inner.borrow_mut();
        if inner.backend.set_speed(speed).is_ok() {
            if let Some(now_playing) = &mut inner.now_playing {
                now_playing.speed = speed;
            }
        }
        inner.publish();
    }

    /// Arms a wall-clock sleep timer: playback pauses once `minutes` have passed, checked once
    /// per tick rather than via a second timer source (see `SleepTimerDeadline`).
    pub fn set_sleep_timer_minutes(&self, minutes: u32) {
        let mut inner = self.inner.borrow_mut();
        let deadline = SleepTimerDeadline::WallClock(Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
        if let Some(now_playing) = &mut inner.now_playing {
            now_playing.sleep_timer = SleepTimerState::Armed(deadline);
        }
        inner.publish();
    }

    /// Arms a sleep timer that fires at the end of whatever chapter is currently playing, falling
    /// back to the end of the item if there's no chapter data (or the position doesn't fall
    /// inside any known chapter).
    pub fn set_sleep_timer_end_of_chapter(&self) {
        let mut inner = self.inner.borrow_mut();
        let position = inner.book_position();
        let Some(now_playing) = &mut inner.now_playing else { return };
        let end = now_playing
            .chapters
            .iter()
            .find(|c| c.start_seconds <= position && position < c.end_seconds)
            .map(|c| c.end_seconds)
            .unwrap_or(now_playing.duration_seconds);
        now_playing.sleep_timer = SleepTimerState::Armed(SleepTimerDeadline::Position(end));
        inner.publish();
    }

    pub fn cancel_sleep_timer(&self) {
        let mut inner = self.inner.borrow_mut();
        if let Some(now_playing) = &mut inner.now_playing {
            now_playing.sleep_timer = SleepTimerState::Off;
        }
        inner.publish();
    }

    fn tick(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.now_playing.is_none() {
            return;
        }

        if let Some(event) = inner.backend.poll_event() {
            match event {
                abs_player::PlayerEvent::EndOfStream => {
                    if let Some((item_id, next_track)) = inner.next_track_after_end_of_stream() {
                        // The next file exists — keep going. The load happens on the main loop
                        // (see `spawn_load_track`); the state machine has already moved on.
                        Inner::spawn_load_track(self.inner.clone(), item_id, next_track, 0.0);
                        // `spawn_load_track` hasn't run `backend.load()` yet — the backend still
                        // reports the *old* pipeline's position, which `book_position()` would
                        // now misattribute to the new (already-bumped) `current_track`'s offset,
                        // producing a bogus overshot position (old-track offset's worth of extra
                        // seconds) for one tick. Returning here instead of falling through to the
                        // unconditional `publish()` below skips that one bad snapshot; the reload
                        // publishes its own correct one (`backend_pos: None` right after `load()`)
                        // moments later. Caught via a real timing-dependent test failure once an
                        // async DB check (`resolve_playable_url`) widened this race's window
                        // enough to make it land inside a test's polling loop.
                        return;
                    } else {
                        let _ = inner.backend.pause();
                        if let Some(now_playing) = &mut inner.now_playing {
                            now_playing.is_playing = false;
                        }
                        // End-of-book is not an unplug pause — a replug must not revive it.
                        inner.paused_by_unplug = false;
                        inner.write_progress(true);
                    }
                }
                abs_player::PlayerEvent::Error(err) => {
                    tracing::warn!(%err, "playback error");
                    let _ = inner.backend.pause();
                    if let Some(now_playing) = &mut inner.now_playing {
                        now_playing.is_playing = false;
                    }
                }
            }
        }

        if let Some(SleepTimerState::Armed(deadline)) = inner.now_playing.as_ref().map(|n| n.sleep_timer) {
            let reached = match deadline {
                SleepTimerDeadline::WallClock(at) => Instant::now() >= at,
                SleepTimerDeadline::Position(end) => inner.book_position() >= end,
            };
            if reached {
                let _ = inner.backend.pause();
                if let Some(now_playing) = &mut inner.now_playing {
                    now_playing.is_playing = false;
                    now_playing.sleep_timer = SleepTimerState::Off;
                }
                // A sleep-timer pause is deliberate; a replug must not override it.
                inner.paused_by_unplug = false;
                inner.write_progress(false);
            }
        }

        inner.publish();

        let is_playing = inner.now_playing.as_ref().is_some_and(|n| n.is_playing);
        if is_playing && inner.last_progress_write.elapsed() >= PROGRESS_WRITE_INTERVAL {
            inner.write_progress(false);
        }
    }
}

pub struct MiniPlayerBar {
    pub root: gtk4::Widget,
    pub controller: PlayerController,
    #[cfg(test)]
    pub hooks: MiniPlayerHooks,
}

#[cfg(test)]
pub struct MiniPlayerHooks {
    pub bar: gtk4::Box,
    pub title_label: gtk4::Label,
    pub author_label: gtk4::Label,
    pub play_button: gtk4::Button,
    pub progress: gtk4::ProgressBar,
}

/// The mini-player bar from `docs/design/ui-spec.md`'s "Player — mini" section: cover placeholder,
/// title/author, play/pause, a thin progress line — hidden until something has actually played.
pub fn build_mini_bar(pool: SqlitePool, paths: AppPaths, backend: Box<dyn abs_player::AudioBackend>) -> MiniPlayerBar {
    let cover = CoverImage::new(40);

    let title_label = gtk4::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    let author_label = gtk4::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    let text_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).hexpand(true).valign(gtk4::Align::Center).build();
    text_box.append(&title_label);
    text_box.append(&author_label);

    let play_icon = gtk4::Image::from_icon_name("media-playback-pause-symbolic");
    let play_button = gtk4::Button::builder().css_classes(["circular", "flat"]).child(&play_icon).build();

    let content_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(10).margin_start(10).margin_end(10).margin_top(6).build();
    content_row.append(cover.widget());
    content_row.append(&text_box);
    content_row.append(&play_button);

    let progress = gtk4::ProgressBar::builder().build();

    let bar = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).css_classes(["toolbar"]).visible(false).build();
    bar.append(&content_row);
    bar.append(&progress);

    let controller = PlayerController::new(pool, paths, backend, {
        let bar = bar.clone();
        let title_label = title_label.clone();
        let author_label = author_label.clone();
        let play_icon = play_icon.clone();
        let progress = progress.clone();
        let cover = cover.clone();
        move |snapshot: &PlayerSnapshot| {
            bar.set_visible(true);
            title_label.set_label(&snapshot.title);
            author_label.set_label(snapshot.author.as_deref().unwrap_or(""));
            author_label.set_visible(snapshot.author.is_some());
            cover.set_path(snapshot.cover_path.as_deref());
            play_icon.set_icon_name(Some(if snapshot.is_playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            }));
            let fraction = if snapshot.duration_seconds > 0.0 {
                (snapshot.position_seconds / snapshot.duration_seconds).clamp(0.0, 1.0)
            } else {
                0.0
            };
            progress.set_fraction(fraction);
        }
    });

    play_button.connect_clicked({
        let controller = controller.clone();
        move |_| controller.toggle_play_pause()
    });

    MiniPlayerBar {
        root: bar.clone().upcast(),
        controller,
        #[cfg(test)]
        hooks: MiniPlayerHooks { bar, title_label, author_label, play_button, progress },
    }
}

/// Implements `abs_player::mpris::MprisCommands` by calling back into a real `PlayerController`
/// — this is the whole dependency-direction resolution for MPRIS: `abs-player`'s `mpris` module
/// only ever calls this trait, never anything from `app` directly.
pub struct MprisBridge {
    controller: PlayerController,
}

impl MprisBridge {
    pub fn new(controller: PlayerController) -> Self {
        Self { controller }
    }
}

impl abs_player::mpris::MprisCommands for MprisBridge {
    fn play_pause(&self) {
        self.controller.toggle_play_pause();
    }
    fn play(&self) {
        self.controller.play();
    }
    fn pause(&self) {
        self.controller.pause();
    }
    fn seek(&self, offset_micros: i64) {
        self.controller.skip(offset_micros as f64 / 1_000_000.0);
    }
    fn set_position(&self, position_micros: i64) {
        self.controller.seek_to_seconds(position_micros as f64 / 1_000_000.0);
    }
    fn next(&self) {
        self.controller.skip(self.controller.skip_intervals().1);
    }
    fn previous(&self) {
        self.controller.skip(-self.controller.skip_intervals().0);
    }
}

/// Converts a snapshot into MPRIS's own state shape — the one place `PlayerSnapshot`'s fields get
/// translated into `xesam:*`/`mpris:*` terms. `mpris:artUrl` needs a `file://` URI, not a plain
/// path, hence `gio::File::for_path(..).uri()`.
pub fn mpris_state_from_snapshot(snapshot: &PlayerSnapshot) -> abs_player::mpris::PlayerState {
    abs_player::mpris::PlayerState {
        status: if snapshot.is_playing { abs_player::mpris::PlaybackStatus::Playing } else { abs_player::mpris::PlaybackStatus::Paused },
        metadata: abs_player::mpris::TrackMetadata {
            title: snapshot.title.clone(),
            artist: snapshot.author.clone(),
            length_micros: (snapshot.duration_seconds * 1_000_000.0) as i64,
            art_url: snapshot.cover_path.as_deref().map(|path| gio::File::for_path(path).uri().to_string()),
        },
        position_micros: (snapshot.position_seconds * 1_000_000.0) as i64,
        rate: snapshot.speed,
    }
}

/// A no-op backend used when `GstBackend::new()` fails (e.g. no GStreamer plugins available) —
/// matches `main.rs`'s existing tolerance for a GStreamer init failure (it warns and continues
/// rather than crashing the whole app over unavailable audio). The mini-player bar and full
/// player screen still build normally; every playback action just logs and does nothing.
struct NullBackend;

impl abs_player::AudioBackend for NullBackend {
    fn load(&mut self, _uri: &str) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
    }
    fn apply_connection(&mut self, _properties: &abs_player::ConnectionProperties) {
        // Nothing to configure: there is no transport behind this backend at all.
    }
    fn play(&mut self) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
    }
    fn pause(&mut self) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
    }
    fn seek(&mut self, _position: Duration) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
    }
    fn set_speed(&mut self, _speed: f64) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
    }
    fn position(&self) -> Option<Duration> {
        None
    }
    fn duration(&self) -> Option<Duration> {
        None
    }
    fn poll_event(&self) -> Option<abs_player::PlayerEvent> {
        None
    }
}

/// The real, production audio backend — `GstBackend::new()` (the system default
/// `autoaudiosink`), falling back to `NullBackend` if GStreamer can't build a pipeline at all.
pub fn real_backend() -> Box<dyn abs_player::AudioBackend> {
    match abs_player::GstBackend::new() {
        Ok(backend) => Box::new(backend),
        Err(err) => {
            tracing::warn!(%err, "couldn't build the audio backend; playback will be unavailable");
            Box::new(NullBackend)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// A real, valid WAV file's bytes — served over HTTP by wiremock so `PlayerController::start`
    /// exercises the actual network fetch (via GStreamer's own HTTP source), not a `file://` URI.
    /// Same fixture trick `abs-player`'s own tests use, just written to an in-memory buffer
    /// instead of disk since this is served, not loaded from a path.
    fn silent_wav_bytes(seconds: u32) -> Vec<u8> {
        let spec = hound::WavSpec { channels: 1, sample_rate: 8000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
            for _ in 0..(spec.sample_rate * seconds) {
                writer.write_sample(0i16).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf.into_inner()
    }

    /// `fakesink` for the audio output (no real hardware in a sandbox), but a real GStreamer
    /// pipeline and a real HTTP fetch otherwise — network liveness and audio-hardware presence
    /// are separate concerns.
    pub(crate) fn test_backend() -> Box<dyn abs_player::AudioBackend> {
        abs_player::init().expect("gstreamer should initialize in this environment");
        Box::new(abs_player::GstBackend::new_with_sink("fakesink").expect("build a playbin with a fake sink"))
    }

    /// Responds to a `Range: bytes=START-[END]` request with `206 Partial Content` and the
    /// matching slice, like the real Audiobookshelf server does (confirmed live: its file
    /// endpoint sends `accept-ranges: bytes`) — GStreamer's `souphttpsrc` issues a byte-range
    /// request for every seek, including the one `PlayerController::start` performs to resume
    /// mid-book, so a mock that ignores `Range` and always replies with the full body from byte 0
    /// would make every resume-from-progress test pass or fail for the wrong reason.
    fn ranged_response(body: Vec<u8>) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync {
        move |req: &Request| {
            let Some(range) = req.headers.get("Range").and_then(|v| v.to_str().ok()) else {
                return ResponseTemplate::new(200).insert_header("Accept-Ranges", "bytes").set_body_bytes(body.clone());
            };
            let Some(spec) = range.strip_prefix("bytes=") else {
                return ResponseTemplate::new(200).insert_header("Accept-Ranges", "bytes").set_body_bytes(body.clone());
            };
            let (start_str, end_str) = spec.split_once('-').unwrap_or((spec, ""));
            let start: usize = start_str.parse().unwrap_or(0);
            let end = if end_str.is_empty() { body.len() - 1 } else { end_str.parse().unwrap_or(body.len() - 1) };
            let end = end.min(body.len() - 1);
            let slice = body[start..=end].to_vec();
            ResponseTemplate::new(206)
                .insert_header("Content-Range", format!("bytes {start}-{end}/{}", body.len()))
                .insert_header("Accept-Ranges", "bytes")
                .set_body_bytes(slice)
        }
    }

    pub(crate) async fn mock_playable_item(mock_server: &MockServer, item_id: &str, seconds: u32) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": f64::from(seconds) }] }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ranged_response(silent_wav_bytes(seconds)))
            .mount(mock_server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/me/progress/{item_id}")))
            .respond_with(ResponseTemplate::new(200))
            .mount(mock_server)
            .await;
    }

    /// Like `mock_playable_item`, but the item detail response also carries a chapter list — for
    /// tests exercising the chapters sheet, including tap-to-seek, which needs real, seekable
    /// audio (not just a parsed `get_item_playback_info` response).
    pub(crate) async fn mock_playable_item_with_chapters(
        mock_server: &MockServer,
        item_id: &str,
        seconds: u32,
        chapters: &[(&str, f64, f64)],
    ) {
        let chapters_json: Vec<_> = chapters
            .iter()
            .enumerate()
            .map(|(index, (title, start, end))| serde_json::json!({ "id": index, "start": start, "end": end, "title": title }))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": f64::from(seconds) }], "chapters": chapters_json }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ranged_response(silent_wav_bytes(seconds)))
            .mount(mock_server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/me/progress/{item_id}")))
            .respond_with(ResponseTemplate::new(200))
            .mount(mock_server)
            .await;
    }

    pub(crate) async fn account_and_server(pool: &SqlitePool, server_url: &str) -> (abs_storage::models::Server, abs_storage::models::Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
        (
            abs_storage::repo::servers::get(pool, &server_id).await.unwrap(),
            abs_storage::repo::accounts::get(pool, &account_id).await.unwrap(),
        )
    }

    /// Progress rows have a foreign key on `(server_id, item_id)` referencing `items`, which
    /// itself references `libraries` — in the real app this is always satisfied (Home only makes
    /// an item clickable once it's synced), so tests exercising `PlayerController::start`'s
    /// progress-write path need to set up the same local rows first.
    pub(crate) async fn insert_synced_item(pool: &SqlitePool, server_id: &str, item_id: &str, title: &str) {
        abs_storage::repo::libraries::upsert(
            pool,
            abs_storage::repo::libraries::UpsertLibrary {
                id: "lib-1",
                server_id,
                name: "Audiobooks",
                media_type: "book",
                icon: None,
                display_order: 1,
            },
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

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point. Covers the whole real
    /// path: resolving a stream target over real HTTP (via wiremock), loading/playing it through a
    /// real GStreamer pipeline, publishing snapshots, pausing, and persisting progress.
    pub(crate) fn run_start_and_pause_persists_progress(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 3));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
            1.0,
        );

        pump_until(|| !seen.borrow().is_empty(), Duration::from_secs(10));
        let first = seen.borrow().last().cloned().expect("starting playback should publish a snapshot");
        assert_eq!(first.title, "Test Book");
        assert_eq!(first.author.as_deref(), Some("Some Author"));
        assert!(first.is_playing, "starting playback should leave it playing");

        controller.pause();
        let after_pause = seen.borrow().last().cloned().unwrap();
        assert!(!after_pause.is_playing, "pause() should be reflected in the next snapshot");

        // `pause()` spawns the actual DB write; pump briefly so it lands before checking.
        pump_until(|| false, Duration::from_millis(300));
        let progress =
            runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap();
        assert!(progress.is_some(), "pausing should persist playback progress");
        assert!(!progress.unwrap().is_finished, "pausing mid-book must not mark it finished");

        // Progress isn't only local — it should also be pushed to the server, so it shows up in
        // the official apps and survives a fresh install.
        let synced_requests = runtime.block_on(mock_server.received_requests()).unwrap();
        let progress_sync = synced_requests
            .iter()
            .find(|r| r.method.as_str() == "PATCH" && r.url.path() == "/api/me/progress/item-1")
            .expect("pausing should also sync progress to the server");
        let body: serde_json::Value = progress_sync.body_json().unwrap();
        assert_eq!(body["isFinished"], false);
        controller.stop();
    }

    /// `add_bookmark` is a plain local repo write with no server sync and no readback anywhere in
    /// the app yet — the only thing worth asserting is that the write actually lands.
    pub(crate) fn run_add_bookmark_persists_a_row(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 10));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        controller.add_bookmark();
        pump_until(|| false, Duration::from_millis(300));

        let count: i64 =
            runtime.block_on(sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks").fetch_one(&pool)).unwrap();
        assert_eq!(count, 1, "add_bookmark should persist a row");
        controller.stop();
    }

    /// Regression test for a real bug caught in manual live testing (not by any prior test — this
    /// path had no coverage at all): `start()` requested a seek immediately after requesting
    /// `pause()`, but a seek needs the pipeline to have *reached* `PAUSED`, not just been asked to
    /// — for a network-streamed source that transition isn't instant, so the seek silently
    /// no-opped and "resume mid-book" quietly restarted from 0 instead. Fixed by polling for
    /// `duration()` to become available (this crate's own signal that PAUSED was actually reached
    /// — see `abs-player`'s own tests) before attempting the resume seek.
    pub(crate) fn run_start_resumes_from_existing_progress(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 10));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(abs_storage::repo::progress::set(&pool, &account.id, &server.id, "item-1", 6.0, false)).unwrap();

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );

        pump_until(|| !seen.borrow().is_empty(), Duration::from_secs(10));
        // A `FLUSH` seek's position update lands on the pipeline's own streaming thread, not
        // synchronously inside `seek()` — give it a brief moment to actually land. 500ms of real
        // (sync'd) playback is nowhere near enough to "catch up" from 0 to past 5s on its own, so
        // this can't pass by coincidence if the resume seek silently no-opped.
        pump_until(|| false, Duration::from_millis(500));
        let position = seen.borrow().last().unwrap().position_seconds;
        assert!(position >= 5.0, "starting an item with existing progress should resume near it, not from 0 (got {position}s)");
        controller.stop();
    }

    /// Like `mock_playable_item`, but the item is split across two audio files — the shape the
    /// multi-track sequencing scenarios need. Both files are served as short, real, ranged WAVs
    /// so end-of-stream handover and cross-file seeking exercise actual GStreamer pipelines.
    pub(crate) async fn mock_two_track_item(mock_server: &MockServer, seconds_per_track: u32) {
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [
                    { "ino": "1", "duration": f64::from(seconds_per_track) },
                    { "ino": "2", "duration": f64::from(seconds_per_track) },
                ] }
            })))
            .mount(mock_server)
            .await;
        for ino in ["1", "2"] {
            Mock::given(method("GET"))
                .and(path(format!("/api/items/item-1/file/{ino}")))
                .respond_with(ranged_response(silent_wav_bytes(seconds_per_track)))
                .mount(mock_server)
                .await;
        }
        Mock::given(method("PATCH"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(mock_server)
            .await;
    }

    /// Regression test: `next_track_after_end_of_stream` corrects later tracks' offsets against
    /// the pipeline's *actual* duration when the server-reported one for the file that just ended
    /// was wrong — this seeds exactly that mismatch (server says 10s, the real WAV is 2s) and
    /// checks the book-level *total* duration is corrected along with the offsets. It's the one
    /// piece of that correction the original implementation missed: the offsets shifted, but
    /// `NowPlaying.duration_seconds` (the total the scrubber, MPRIS, and `mark_as_finished`/
    /// `seek_to_seconds`'s clamp all read) stayed at the stale, server-reported sum.
    pub(crate) fn run_track_duration_correction_updates_the_book_total(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/api/items/item-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    // Track 1 is declared as 10s but the WAV actually served is only 2s — the
                    // kind of inaccurate server metadata this correction exists to tolerate.
                    "media": { "audioFiles": [
                        { "ino": "1", "duration": 10.0 },
                        { "ino": "2", "duration": 2.0 },
                    ] }
                })))
                .mount(&mock_server)
                .await;
            for ino in ["1", "2"] {
                Mock::given(method("GET"))
                    .and(path(format!("/api/items/item-1/file/{ino}")))
                    .respond_with(ranged_response(silent_wav_bytes(2)))
                    .mount(&mock_server)
                    .await;
            }
            Mock::given(method("PATCH"))
                .and(path("/api/me/progress/item-1"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&mock_server)
                .await;
        });

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        // Before the first track ends, the book total is still the stale server-reported sum
        // (10 + 2 = 12s) — nothing has had a reason to correct it yet.
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));
        assert_eq!(controller.snapshot().unwrap().duration_seconds, 12.0);

        // Once the real (2s) first track ends and hands over to the second, the correction fires:
        // the true book total is 2 (corrected track 1) + 2 (track 2) = 4s, not the stale 12s.
        pump_until(
            || {
                let requests = runtime.block_on(mock_server.received_requests()).unwrap();
                requests.iter().any(|r| r.method.as_str() == "GET" && r.url.path() == "/api/items/item-1/file/2")
            },
            Duration::from_secs(15),
        );
        pump_until(|| false, Duration::from_millis(200));
        let duration = controller.snapshot().unwrap().duration_seconds;
        assert!((duration - 4.0).abs() < 0.5, "book total should be corrected to ~4s, got {duration}");

        controller.stop();
    }

    /// Seeds the `tracks` rows (the cached per-file metadata `sync_item_tracks` normally writes
    /// after resolving) for an item's full track list in one go — one call, not one per track,
    /// since `upsert_all` *replaces* the item's whole list.
    async fn seed_track_metadata(pool: &SqlitePool, server_id: &str, item_id: &str, tracks: &[(&str, f64, f64)]) {
        let rows: Vec<abs_storage::repo::tracks::NewTrack<'_>> = tracks
            .iter()
            .map(|&(ino, duration_seconds, offset_seconds)| abs_storage::repo::tracks::NewTrack { ino, duration_seconds, offset_seconds, size_bytes: None })
            .collect();
        abs_storage::repo::tracks::upsert_all(pool, server_id, item_id, &rows).await.unwrap();
    }

    /// Seeds a `Complete` `download_tracks` row for `ino` pointing at a real file written with
    /// `bytes` — the exact "trustworthy" shape `local_track_path`/`verified_complete_path` require
    /// (status `Complete`, on-disk size matching `expected_size_bytes`). The item's `tracks` rows
    /// must already exist (via `seed_track_metadata` — the download table has a foreign key on
    /// them, normally written by `sync_item_tracks` before any download starts).
    async fn seed_downloaded_track(pool: &SqlitePool, paths: &AppPaths, server_id: &str, item_id: &str, ino: &str, bytes: &[u8]) {
        let path = paths.track_file_path(server_id, item_id, ino, "wav");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, bytes).await.unwrap();
        abs_storage::repo::download_tracks::upsert_pending(pool, server_id, item_id, ino, path.to_str().unwrap()).await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(pool, server_id, item_id, ino, bytes.len() as i64).await.unwrap();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. A track that's already been
    /// downloaded (a `Complete` row with a real, correctly-sized file on disk) must be played from
    /// that local file instead of streaming it again — asserted by checking the file endpoint was
    /// never actually requested, not just that playback worked.
    pub(crate) fn run_downloaded_track_is_preferred_over_streaming(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 3));

        let pool = runtime.block_on(pool());
        let paths = crate::test_support::test_paths();
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(seed_track_metadata(&pool, &server.id, "item-1", &[("1", 3.0, 0.0)]));
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "1", &silent_wav_bytes(3)));

        let controller = PlayerController::new(pool.clone(), paths, test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool, &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some_and(|s| s.duration_seconds > 0.0), Duration::from_secs(10));
        // Give playback a real moment to run — long enough that a streamed track would definitely
        // have issued its HTTP request by now.
        pump_until(|| false, Duration::from_millis(500));

        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            !requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/1"),
            "a downloaded track must be played from disk, not re-streamed"
        );
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. A `Complete` row whose file has gone
    /// missing (deleted externally after the row was written) must not be trusted — playback falls
    /// back to streaming rather than failing to load anything.
    pub(crate) fn run_untrustworthy_complete_row_falls_back_to_streaming(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 3));

        let pool = runtime.block_on(pool());
        let paths = crate::test_support::test_paths();
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(seed_track_metadata(&pool, &server.id, "item-1", &[("1", 3.0, 0.0)]));
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "1", &silent_wav_bytes(3)));
        // Delete the file after the row was written — the row still says `Complete`, but it's a
        // lie now.
        let stale_path = paths.track_file_path(&server.id, "item-1", "1", "wav");
        runtime.block_on(tokio::fs::remove_file(&stale_path)).unwrap();

        let controller = PlayerController::new(pool.clone(), paths, test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool, &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(
            || runtime.block_on(mock_server.received_requests()).unwrap().iter().any(|r| r.url.path() == "/api/items/item-1/file/1"),
            Duration::from_secs(10),
        );
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/1"),
            "an untrustworthy Complete row (missing file) must fall back to streaming"
        );
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Mixed state across a two-track item:
    /// the first track is downloaded, the second isn't. Starting plays the first from disk (no
    /// request), and crossing into the second (via `spawn_load_track`) streams it (a request).
    pub(crate) fn run_multi_track_mixed_downloaded_and_streamed(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, 2));

        let pool = runtime.block_on(pool());
        let paths = crate::test_support::test_paths();
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(seed_track_metadata(&pool, &server.id, "item-1", &[("1", 2.0, 0.0)]));
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "1", &silent_wav_bytes(2)));

        let controller = PlayerController::new(pool.clone(), paths, test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool, &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        // The first track plays from disk — give it a real moment, then confirm no request for
        // its file landed, before letting it run on into the second (streamed) track.
        pump_until(|| controller.snapshot().is_some_and(|s| s.duration_seconds > 0.0), Duration::from_secs(10));
        pump_until(|| false, Duration::from_millis(500));
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(!requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/1"), "the downloaded first track must not be streamed");

        pump_until(
            || runtime.block_on(mock_server.received_requests()).unwrap().iter().any(|r| r.url.path() == "/api/items/item-1/file/2"),
            Duration::from_secs(15),
        );
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/2"), "the not-yet-downloaded second track must be streamed");
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Offline start of a fully-downloaded
    /// item: the server can't even serve the item's playback info (nothing is mounted, so
    /// wiremock 404s it), but every track is on disk — playback must start entirely from the
    /// local files, with the book duration coming from the locally cached track metadata, and
    /// never touch the server's file endpoint.
    pub(crate) fn run_fully_downloaded_item_plays_offline(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());

        let pool = runtime.block_on(pool());
        let paths = crate::test_support::test_paths();
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(seed_track_metadata(&pool, &server.id, "item-1", &[("1", 3.0, 0.0), ("2", 2.0, 3.0)]));
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "1", &silent_wav_bytes(3)));
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "2", &silent_wav_bytes(2)));

        let controller = PlayerController::new(pool.clone(), paths, test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool, &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Offline Book".to_string(), author: None },
            1.0,
        );

        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));
        let snapshot = controller.snapshot().unwrap();
        assert!(snapshot.is_playing, "a fully-downloaded item must play offline");
        assert_eq!(snapshot.duration_seconds, 5.0, "the book total must come from the locally cached track metadata, not the failed resolve");

        // Give playback a real moment — long enough that a streamed track would definitely have
        // issued its HTTP request by now — then confirm it never did.
        pump_until(|| false, Duration::from_millis(500));
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            !requests.iter().any(|r| r.url.path().starts_with("/api/items/item-1/file/")),
            "a fully-downloaded item must play entirely offline"
        );
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Offline start of a partially-
    /// downloaded item: the downloaded first track plays from disk; when it ends and playback
    /// crosses into the not-downloaded second track, the streaming load fails (the server is
    /// unreachable) and playback stops at the gap through the existing error path — instead of
    /// the item failing to start at all.
    pub(crate) fn run_partially_downloaded_item_plays_until_a_gap_offline(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());

        let pool = runtime.block_on(pool());
        let paths = crate::test_support::test_paths();
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(seed_track_metadata(&pool, &server.id, "item-1", &[("1", 2.0, 0.0), ("2", 2.0, 2.0)]));
        // Only the first track is downloaded; the second has metadata but no file, no download row.
        runtime.block_on(seed_downloaded_track(&pool, &paths, &server.id, "item-1", "1", &silent_wav_bytes(2)));

        let controller = PlayerController::new(pool.clone(), paths, test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool, &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Half Offline Book".to_string(), author: None },
            1.0,
        );

        // The downloaded track plays from disk — never streamed.
        pump_until(|| controller.snapshot().is_some_and(|s| s.duration_seconds > 0.0), Duration::from_secs(10));
        pump_until(|| false, Duration::from_millis(500));
        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(!requests.iter().any(|r| r.url.path() == "/api/items/item-1/file/1"), "the downloaded track must not be streamed");

        // Crossing into the gap reaches for the server, fails, and stops playback.
        pump_until(
            || runtime.block_on(mock_server.received_requests()).unwrap().iter().any(|r| r.url.path() == "/api/items/item-1/file/2"),
            Duration::from_secs(15),
        );
        pump_until(|| !controller.snapshot().is_some_and(|s| s.is_playing), Duration::from_secs(10));
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point. Multi-file items used to
    /// stop dead (and mark the whole book finished) at the first file's end, with a caveat note
    /// saying so; the scenarios that follow pin the sequential playback that replaced that.
    pub(crate) fn run_multi_track_advances_to_the_next_track(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, 2));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        // Track 1 is 2s: the book-level position must keep going past it into the second track's
        // range, not stop at the boundary.
        pump_until(|| seen.borrow().last().is_some_and(|s| s.position_seconds >= 2.5), Duration::from_secs(15));
        assert!(seen.borrow().last().unwrap().is_playing, "advancing to the next track should not stop playback");

        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            requests.iter().any(|r| r.method.as_str() == "GET" && r.url.path() == "/api/items/item-1/file/2"),
            "the second file should actually have been fetched"
        );

        controller.pause();
        pump_until(|| false, Duration::from_millis(300));
        let progress =
            runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap();
        assert!(!progress.unwrap().is_finished, "reaching the first track's end must not mark the book finished");
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Only the *last* track's end-of-stream
    /// is the book's end: a very short two-track clip so both handover and the final stop arrive
    /// quickly, then progress must read finished at the book-level end.
    pub(crate) fn run_multi_track_final_track_marks_finished(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, 1));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        // `is_playing` stays true through the first track's handover, so the first false reading
        // is the final end-of-stream.
        pump_until(|| seen.borrow().last().is_some_and(|s| !s.is_playing), Duration::from_secs(15));

        pump_until(|| false, Duration::from_millis(300));
        let progress =
            runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap().unwrap();
        assert!(progress.is_finished, "the final track's end should mark the book finished");
        assert!(
            progress.current_time_seconds >= 1.9,
            "should be recorded at the book-level end, got {}",
            progress.current_time_seconds
        );
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Seeking to a book-level position
    /// inside the second file must load that file and land the pipeline at the mapped in-track
    /// offset — asserted while paused, so a correct seek *stays* at its target rather than
    /// drifting (which is also what keeps this from passing by natural playback reaching the
    /// target on its own).
    pub(crate) fn run_seek_across_track_boundary_lands_in_the_next_file(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, 3));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some_and(|s| s.position_seconds > 0.5), Duration::from_secs(10));
        controller.pause();
        pump_until(|| controller.snapshot().is_some_and(|s| !s.is_playing), Duration::from_secs(5));

        controller.seek_to_seconds(4.0); // 1s into the second 3s file
        pump_until(|| controller.snapshot().is_some_and(|s| s.position_seconds >= 3.9), Duration::from_secs(10));
        let position = controller.snapshot().unwrap().position_seconds;
        assert!(position <= 4.5, "a paused cross-track seek should land at its target and stay there (got {position})");

        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            requests.iter().any(|r| r.method.as_str() == "GET" && r.url.path() == "/api/items/item-1/file/2"),
            "seeking into the second track should fetch the second file"
        );
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Book-level progress saved by another
    /// client (or an earlier session) can fall inside a later file: resuming at 6s of a 5s+5s
    /// book must load the second file directly at its in-track offset — and, the part the old
    /// first-file-only behavior got wrong, must not fetch the first file at all.
    pub(crate) fn run_resume_jumps_straight_to_the_second_track(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_two_track_item(&mock_server, 5));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));
        runtime.block_on(abs_storage::repo::progress::set(&pool, &account.id, &server.id, "item-1", 6.0, false)).unwrap();

        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        pump_until(|| controller.snapshot().is_some_and(|s| s.position_seconds >= 5.9), Duration::from_secs(15));
        let position = controller.snapshot().unwrap().position_seconds;
        assert!(position <= 7.0, "resuming should land near the saved book position, got {position}");

        let requests = runtime.block_on(mock_server.received_requests()).unwrap();
        assert!(
            !requests.iter().any(|r| r.method.as_str() == "GET" && r.url.path() == "/api/items/item-1/file/1"),
            "resuming into the second track must not fetch the first file"
        );
        assert!(
            requests.iter().any(|r| r.method.as_str() == "GET" && r.url.path() == "/api/items/item-1/file/2"),
            "resuming into the second track should fetch the second file"
        );
        controller.stop();
    }

    pub(crate) fn run_end_of_stream_pauses_and_marks_finished(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        // A very short clip so end-of-stream arrives quickly.
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 1));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Short Clip".to_string(), author: None },
            1.0,
        );

        pump_until(|| !seen.borrow().is_empty(), Duration::from_secs(10));
        pump_until(|| seen.borrow().last().is_some_and(|s| !s.is_playing), Duration::from_secs(10));

        assert!(
            !seen.borrow().last().unwrap().is_playing,
            "reaching end-of-stream should leave playback paused"
        );

        pump_until(|| false, Duration::from_millis(300));
        let progress =
            runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap();
        assert!(
            progress.is_some_and(|p| p.is_finished),
            "reaching end-of-stream should persist progress marked finished"
        );
        controller.stop();
    }

    /// The mini-player bar's own widget wiring (`build_mini_bar`) is otherwise untested by every
    /// scenario above, which all drive a bare `PlayerController` directly. Covers: hidden until
    /// something plays, then visible with the right title/author, a live progress fraction, and
    /// the play/pause button toggling real controller state.
    pub(crate) fn run_mini_bar_reflects_playback_state(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 3));

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let mini_bar = build_mini_bar(pool.clone(), crate::test_support::test_paths(), test_backend());
        let hooks = &mini_bar.hooks;
        assert!(!hooks.bar.is_visible(), "the mini bar should stay hidden until something plays");

        mini_bar.controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
            1.0,
        );
        pump_until(|| hooks.bar.is_visible(), Duration::from_secs(10));

        assert_eq!(hooks.title_label.label(), "Test Book");
        assert_eq!(hooks.author_label.label(), "Some Author");

        pump_until(|| hooks.progress.fraction() > 0.0, Duration::from_secs(5));
        assert!(hooks.progress.fraction() > 0.0, "the progress line should reflect a live position");

        hooks.play_button.emit_clicked();
        pump_until(|| !mini_bar.controller.snapshot().unwrap().is_playing, Duration::from_secs(5));
        assert!(
            !mini_bar.controller.snapshot().unwrap().is_playing,
            "the mini bar's play/pause button should control the real controller"
        );
        mini_bar.controller.stop();
    }
}
