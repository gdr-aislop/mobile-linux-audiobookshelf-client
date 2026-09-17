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

use abs_storage::models::{Account, Server};
use abs_storage::AppPaths;

use crate::widgets::cover_image::CoverImage;

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
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub is_playing: bool,
    pub multi_track_note: Option<String>,
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
    server_url: String,
    access_token: String,
    title: String,
    author: Option<String>,
    duration_seconds: f64,
    multi_track_note: Option<String>,
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
}

impl Inner {
    fn snapshot(&self) -> Option<PlayerSnapshot> {
        let now_playing = self.now_playing.as_ref()?;
        Some(PlayerSnapshot {
            title: now_playing.title.clone(),
            author: now_playing.author.clone(),
            position_seconds: self.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0),
            duration_seconds: now_playing.duration_seconds,
            is_playing: now_playing.is_playing,
            multi_track_note: now_playing.multi_track_note.clone(),
            speed: now_playing.speed,
            sleep_timer_active: now_playing.sleep_timer != SleepTimerState::Off,
            cover_path: now_playing.cover_path.clone(),
        })
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
    /// re-reading `self` from inside the spawned future.
    fn write_progress(&mut self, is_finished: bool) {
        let Some(now_playing) = &self.now_playing else { return };
        let position = self.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let pool = self.pool.clone();
        let account_id = now_playing.account_id.clone();
        let server_id = now_playing.server_id.clone();
        let item_id = now_playing.item_id.clone();
        let server_url = now_playing.server_url.clone();
        let access_token = now_playing.access_token.clone();
        let duration_seconds = now_playing.duration_seconds;
        self.last_progress_write = Instant::now();

        glib::spawn_future_local(async move {
            if let Err(err) = abs_storage::repo::progress::set(&pool, &account_id, &server_id, &item_id, position, is_finished).await
            {
                tracing::warn!(%err, "couldn't persist playback progress");
            }
            // Best-effort: the local write above is this client's own source of truth (Home's
            // "Continue Listening" reads it), so a network hiccup syncing it up to the server
            // must not be treated as a playback error.
            if let Err(err) =
                abs_core::streaming::sync_progress_to_server(&server_url, &access_token, &item_id, position, duration_seconds, is_finished)
                    .await
            {
                tracing::warn!(%err, "couldn't sync playback progress to the server");
            }
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
    /// currently needs to stop listening once registered; MPRIS, once wired, will use this).
    #[allow(dead_code, reason = "unused until MPRIS wiring lands; part of this phase's foundation refactor")]
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
        let position = inner.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        drop(inner);

        glib::spawn_future_local(async move {
            if let Err(err) = abs_storage::repo::bookmarks::add(&pool, &account_id, &server_id, &item_id, position).await {
                tracing::warn!(%err, "couldn't save bookmark");
            }
        });
    }

    /// Resolves a playable URL and starts playback, resuming from any existing progress for this
    /// item/account rather than always starting over from position 0. `default_speed` is
    /// `PlaybackSettings::default_speed`, applied once at the start of every session (the user can
    /// change it afterwards via `set_speed`).
    pub fn start(&self, server: Server, account: Account, item: PlayRequest, default_speed: f64) {
        let inner_rc = self.inner.clone();
        glib::spawn_future_local(async move {
            let (pool, paths) = {
                let inner = inner_rc.borrow();
                (inner.pool.clone(), inner.paths.clone())
            };

            // Resolving the stream URL is required to proceed; reconciling progress and fetching
            // the cover are both nice-to-haves that must never add their own delay on top — run
            // all three concurrently (each already has its own bounded timeout) rather than one
            // after another, so a slow or unreachable server is only ever felt once, not thrice.
            let (target_result, reconcile_result, cover_path) = tokio::join!(
                abs_core::streaming::resolve_stream_target(&server.url, &account.token, &item.item_id),
                abs_core::progress_sync::reconcile_item_progress(&pool, &server.url, &account.token, &account.id, &server.id, &item.item_id),
                abs_core::covers::fetch_and_cache_cover(&paths, &pool, &server.url, &account.token, &server.id, &item.item_id),
            );
            if let Err(err) = reconcile_result {
                tracing::warn!(%err, item_id = %item.item_id, "couldn't reconcile progress with the server; using local progress");
            }
            let target = match target_result {
                Ok(target) => target,
                Err(err) => {
                    tracing::warn!(%err, item_id = %item.item_id, "couldn't resolve a playable URL");
                    return;
                }
            };

            // Reads whatever `reconcile_item_progress` just wrote, if it succeeded — falling back
            // to this client's own last local write (or nothing) otherwise.
            let resume_at = abs_storage::repo::progress::get(&pool, &account.id, &server.id, &item.item_id)
                .await
                .ok()
                .flatten()
                .map(|p| p.current_time_seconds)
                .filter(|s| *s > 0.0 && *s < target.duration_seconds);

            let multi_track_note = (target.track_count > 1).then(|| {
                format!(
                    "Playing track 1 of {} — full multi-track playback isn't supported yet.",
                    target.track_count
                )
            });

            // Local DB write only, no network involved — inline rather than part of the
            // `tokio::join!` above, which is reserved for concurrent network calls.
            if let Err(err) = abs_core::chapters::sync_item_chapters(&pool, &server.id, &item.item_id, &target.chapters).await {
                tracing::warn!(%err, item_id = %item.item_id, "couldn't persist chapters locally");
            }
            let chapters = target
                .chapters
                .iter()
                .map(|c| ChapterInfo { title: c.title.clone(), start_seconds: c.start_seconds, end_seconds: c.end_seconds })
                .collect();

            {
                let mut inner = inner_rc.borrow_mut();
                if let Err(err) = inner.backend.load(&target.url) {
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
            let needs_seek_ready = resume_at.is_some() || (default_speed - 1.0).abs() > f64::EPSILON;
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
            if let Some(resume_at) = resume_at {
                let _ = inner.backend.seek(Duration::from_secs_f64(resume_at));
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
                server_id: server.id,
                account_id: account.id,
                server_url: server.url,
                access_token: account.token,
                title: item.title,
                author: item.author,
                duration_seconds: target.duration_seconds,
                multi_track_note,
                is_playing: true,
                chapters,
                speed: applied_speed,
                sleep_timer: SleepTimerState::Off,
                cover_path,
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
        if inner.backend.pause().is_ok() {
            if let Some(now_playing) = &mut inner.now_playing {
                now_playing.is_playing = false;
            }
        }
        inner.publish();
        inner.write_progress(false);
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
        let mut inner = self.inner.borrow_mut();
        let Some(now_playing) = &inner.now_playing else { return };
        let duration = now_playing.duration_seconds;
        let current = inner.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let target = (current + delta_seconds).clamp(0.0, duration);
        let _ = inner.backend.seek(Duration::from_secs_f64(target));
        inner.publish();
    }

    pub fn seek_fraction(&self, fraction: f64) {
        let Some(duration_seconds) = self.inner.borrow().now_playing.as_ref().map(|np| np.duration_seconds) else { return };
        self.seek_to_seconds(fraction.clamp(0.0, 1.0) * duration_seconds);
    }

    /// Seeks to an absolute position — the primitive behind `seek_fraction`, and used directly by
    /// the chapters sheet (tap-to-seek to a chapter's start) and MPRIS `Seek`/`SetPosition`.
    pub fn seek_to_seconds(&self, seconds: f64) {
        let mut inner = self.inner.borrow_mut();
        let Some(now_playing) = &inner.now_playing else { return };
        let target = seconds.clamp(0.0, now_playing.duration_seconds);
        let _ = inner.backend.seek(Duration::from_secs_f64(target));
        inner.publish();
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
        let position = inner.backend.position().map(|d| d.as_secs_f64()).unwrap_or(0.0);
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
                    let _ = inner.backend.pause();
                    if let Some(now_playing) = &mut inner.now_playing {
                        now_playing.is_playing = false;
                    }
                    inner.write_progress(true);
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
                SleepTimerDeadline::Position(end) => {
                    inner.backend.position().is_some_and(|p| p.as_secs_f64() >= end)
                }
            };
            if reached {
                let _ = inner.backend.pause();
                if let Some(now_playing) = &mut inner.now_playing {
                    now_playing.is_playing = false;
                    now_playing.sleep_timer = SleepTimerState::Off;
                }
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

/// A no-op backend used when `GstBackend::new()` fails (e.g. no GStreamer plugins available) —
/// matches `main.rs`'s existing tolerance for a GStreamer init failure (it warns and continues
/// rather than crashing the whole app over unavailable audio). The mini-player bar and full
/// player screen still build normally; every playback action just logs and does nothing.
struct NullBackend;

impl abs_player::AudioBackend for NullBackend {
    fn load(&mut self, _uri: &str) -> abs_player::Result<()> {
        Err(abs_player::PlayerError::NoSourceLoaded)
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

    pub(crate) async fn account_and_server(pool: &SqlitePool, server_url: &str) -> (Server, Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123").await.unwrap();
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
            server.clone(),
            account.clone(),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
            1.0,
        );

        pump_until(|| !seen.borrow().is_empty(), Duration::from_secs(10));
        let first = seen.borrow().last().cloned().expect("starting playback should publish a snapshot");
        assert_eq!(first.title, "Test Book");
        assert_eq!(first.author.as_deref(), Some("Some Author"));
        assert!(first.is_playing, "starting playback should leave it playing");
        assert!(first.multi_track_note.is_none(), "a single-file item has no multi-track caveat");

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
            server.clone(),
            account.clone(),
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
        let controller = PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            server,
            account,
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

    pub(crate) fn run_multi_track_item_sets_a_caveat_note(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/api/items/item-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "media": { "audioFiles": [
                        { "ino": "1", "duration": 3.0 },
                        { "ino": "2", "duration": 3.0 },
                    ] }
                })))
                .mount(&mock_server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/items/item-1/file/1"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(silent_wav_bytes(3)))
                .mount(&mock_server)
                .await;
        });

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let seen: Rc<RefCell<Vec<PlayerSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
        let controller = PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), {
            let seen = seen.clone();
            move |snapshot: &PlayerSnapshot| seen.borrow_mut().push(snapshot.clone())
        });

        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
        );

        pump_until(|| !seen.borrow().is_empty(), Duration::from_secs(10));
        let snapshot = seen.borrow().last().cloned().unwrap();
        assert!(snapshot.multi_track_note.is_some(), "a 2-file item should carry a multi-track caveat");
        assert!(snapshot.multi_track_note.unwrap().contains('2'));
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
            server.clone(),
            account.clone(),
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

        let mini_bar = build_mini_bar(pool, crate::test_support::test_paths(), test_backend());
        let hooks = &mini_bar.hooks;
        assert!(!hooks.bar.is_visible(), "the mini bar should stay hidden until something plays");

        mini_bar.controller.start(
            server,
            account,
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
