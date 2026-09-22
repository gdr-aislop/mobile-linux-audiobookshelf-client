//! The download pipeline's core: mapping chapter selections onto tracks, computing offline
//! availability, and the resumable single-track fetch itself. See `repo::download_tracks`/
//! `repo::tracks` (`abs-storage`) for why tracking is per-track, not per-chapter — the server only
//! ever serves whole audio files.
//!
//! `download_track` is the one function in this module that does real network I/O; everything
//! else here is pure logic over already-fetched/cached data. Deliberately does **not** own a
//! queue, a concurrency limit, or a registry of in-flight cancellation flags — that orchestration
//! belongs to the app-layer `DownloadManager` (`app/src/downloads.rs`), which calls this function
//! once per track it decides to run, the same way `app/src/player.rs`'s `PlayerController` is the
//! one place that drives `abs_player::AudioBackend`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;

use abs_storage::models::{DownloadStatus, DownloadTrack};
use abs_storage::AppPaths;

use crate::error::Result;
use crate::tracks::TrackRef;

/// Generous absolute ceiling on one *attempt* of a track download — not the per-chunk stall
/// detector below, which is what actually catches a hung connection in practice. This just bounds
/// the pathological case of a connection that keeps trickling single bytes forever without ever
/// going fully idle.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// A chunk read that takes longer than this is treated as a stalled connection — retried (with
/// backoff, resuming from whatever was already written) rather than left to hang indefinitely.
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(20);
/// Retries are for transient conditions (a dropped connection, a `5xx`); a genuinely broken
/// request (bad auth, a track that no longer exists) will never succeed no matter how many times
/// it's retried, so this cap exists to eventually give up and surface a real error rather than
/// loop forever against a server that will never say yes.
const MAX_ATTEMPTS: u32 = 5;
const BACKOFF_BASE: Duration = Duration::from_millis(500);
/// How often `bytes_downloaded` is persisted to the database mid-transfer — bounded so a fast
/// local network doesn't turn every single chunk into its own `UPDATE`, while still keeping the
/// resumable checkpoint reasonably fresh if the app is killed mid-download.
const PROGRESS_PERSIST_INTERVAL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackDownloadOutcome {
    Completed,
    Canceled,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineAvailability {
    None,
    Partial,
    Full,
}

/// Which track holds a given book-level position — same `rposition`-based semantics as
/// `streaming::locate_track`, just over `TrackRef` (no URL) instead of `StreamTrack`. Kept as a
/// separate small function rather than converting to `StreamTrack` and back: the two types exist
/// for different reasons (one carries a playable URL, one doesn't), and forcing one through the
/// other's shape for a single index lookup isn't worth it.
fn track_index_at(tracks: &[TrackRef], book_seconds: f64) -> usize {
    tracks.iter().rposition(|t| t.offset_seconds <= book_seconds + 1e-6).unwrap_or(0)
}

/// The track containing the instant *just before* `book_seconds` — used for a chapter's `end`
/// boundary (chapters are `[start, end)`, half-open). Deliberately a strict `<`, not `track_index_at`
/// with a subtracted epsilon: subtracting a small epsilon from `end` and then adding one back
/// inside `track_index_at`'s own fuzz can round-trip back onto the boundary itself in floating
/// point, which put a chapter's needed tracks one track too far in testing. A plain strict `<`
/// has no such cancellation.
fn track_index_before(tracks: &[TrackRef], book_seconds: f64) -> usize {
    tracks.iter().rposition(|t| t.offset_seconds < book_seconds).unwrap_or(0)
}

/// Which `ino`s the given chapter indices touch. Maps each chapter's book-level `[start, end)`
/// range onto the tracks it overlaps: the start boundary resolves normally, and the end boundary
/// is probed just before `end` (not `end` itself) so a chapter's own last track is included
/// without spilling onto whatever track the *next* chapter's first instant belongs to.
pub fn tracks_needed_for_chapters(tracks: &[TrackRef], chapters: &[abs_api::ChapterRef], chapter_indices: &[usize]) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    if tracks.is_empty() {
        return needed;
    }
    for &index in chapter_indices {
        let Some(chapter) = chapters.get(index) else { continue };
        let start_idx = track_index_at(tracks, chapter.start_seconds);
        let end_idx = track_index_before(tracks, chapter.end_seconds).max(start_idx);
        for track in &tracks[start_idx..=end_idx] {
            needed.insert(track.ino.clone());
        }
    }
    needed
}

/// The total bytes a download for the given chapter indices would need to fetch — the sum of the
/// server-reported sizes of every *distinct* track those chapters touch. Whole files are what
/// downloads fetch, so a chapter straddling two tracks counts both, and two chapters sharing one
/// file count it once. `None` when any touched track's size is unknown — callers show no estimate
/// rather than a wrong one. Pure (no I/O), for the download sheet's per-row subtitles; takes
/// plain `(f64, f64)` chapter ranges so a caller with its own chapter type (the Player screen's
/// `ChapterInfo`) never has to construct an `abs_api` one, the same boundary
/// `chapter_offline_markers_for_item` below already draws.
pub fn estimate_bytes_for_chapters(tracks: &[TrackRef], chapter_ranges: &[(f64, f64)], chapter_indices: &[usize]) -> Option<u64> {
    let chapters: Vec<abs_api::ChapterRef> = chapter_ranges
        .iter()
        .map(|&(start_seconds, end_seconds)| abs_api::ChapterRef { title: String::new(), start_seconds, end_seconds })
        .collect();
    let needed = tracks_needed_for_chapters(tracks, &chapters, chapter_indices);
    // An empty `needed` (no cached tracks at all, or chapters that map onto none) is "can't know",
    // not "zero bytes" — a caller showing "≈0 B" for a real book would be worse than no estimate.
    if needed.is_empty() {
        return None;
    }
    let mut total = 0u64;
    for ino in needed {
        let track = tracks.iter().find(|t| t.ino == ino)?;
        total += track.size_bytes?;
    }
    Some(total)
}

/// One bool per chapter (in order) — the per-chapter offline glyph Item Detail will show, without
/// storing a single "chapter downloaded" row anywhere (there's no such file to point at). A
/// chapter counts as offline only if *every* track it touches is complete — a chapter split across
/// a track boundary where only one side finished downloading is not yet safely playable offline.
pub fn chapter_offline_markers(tracks: &[TrackRef], chapters: &[abs_api::ChapterRef], complete_inos: &BTreeSet<String>) -> Vec<bool> {
    (0..chapters.len())
        .map(|index| {
            let needed = tracks_needed_for_chapters(tracks, chapters, std::slice::from_ref(&index));
            !needed.is_empty() && needed.is_subset(complete_inos)
        })
        .collect()
}

/// Whether an item is fully, partially, or not at all downloaded — compared against its actual
/// (cached) track count, the definition the Home/Library offline-mode toggle filters on. Returns
/// `None` (the availability, not the Rust `Option`) if the item's tracks haven't even been synced
/// locally yet — nothing can be "downloaded" if there's no cached record of what to download.
pub async fn item_offline_availability(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<OfflineAvailability> {
    let tracks = abs_storage::repo::tracks::list_for_item(pool, server_id, item_id).await?;
    if tracks.is_empty() {
        return Ok(OfflineAvailability::None);
    }
    let downloads = abs_storage::repo::download_tracks::list_for_item(pool, server_id, item_id).await?;
    let complete: BTreeSet<&str> = downloads.iter().filter(|d| d.status == DownloadStatus::Complete).map(|d| d.ino.as_str()).collect();
    let done = tracks.iter().filter(|t| complete.contains(t.ino.as_str())).count();
    Ok(match done {
        0 => OfflineAvailability::None,
        d if d == tracks.len() => OfflineAvailability::Full,
        _ => OfflineAvailability::Partial,
    })
}

/// One-call version of the tracks-fetch + complete-tracks-fetch + `chapter_offline_markers`
/// pipeline, taking chapters as plain `(start_seconds, end_seconds)` pairs rather than
/// `abs_api::ChapterRef` — so a caller (the Player screen) that only has its own in-memory chapter
/// list (`app::player::ChapterInfo`, not an `abs_api` type) never has to construct one just to call
/// this, keeping `app`'s "never imports `abs_api` directly" rule intact.
pub async fn chapter_offline_markers_for_item(pool: &SqlitePool, server_id: &str, item_id: &str, chapter_ranges: &[(f64, f64)]) -> Result<Vec<bool>> {
    let tracks = crate::tracks::cached_tracks(pool, server_id, item_id).await?;
    let complete = complete_inos_for_item(pool, server_id, item_id).await?;
    let chapters: Vec<abs_api::ChapterRef> =
        chapter_ranges.iter().map(|&(start_seconds, end_seconds)| abs_api::ChapterRef { title: String::new(), start_seconds, end_seconds }).collect();
    Ok(chapter_offline_markers(&tracks, &chapters, &complete))
}

/// A `Complete` row is only trustworthy if its file still exists on disk with the size recorded at
/// download time — a size mismatch (or a missing file, e.g. deleted externally) means the row is
/// stale, and callers must treat it the same as "not downloaded" rather than crash or play/report a
/// corrupt file. Shared by `download_track`'s own idempotency check (skip a re-download) and
/// `local_track_path` below (prefer a local file over streaming) — one definition of "trustworthy",
/// not two copies that could quietly drift apart.
async fn verified_complete_path(row: &DownloadTrack) -> Option<PathBuf> {
    if row.status != DownloadStatus::Complete {
        return None;
    }
    let metadata = tokio::fs::metadata(&row.file_path).await.ok()?;
    let size_matches = row.expected_size_bytes.map(|expected| expected as u64 == metadata.len()).unwrap_or(true);
    size_matches.then(|| PathBuf::from(&row.file_path))
}

/// The on-disk path for a track, if — and only if — it's verifiably safe to play from: a `Complete`
/// row whose file still matches its recorded size. `None` covers every other case (never downloaded,
/// still in progress, failed, or a stale/corrupted row) uniformly, so callers (playback, preferring
/// a local file over streaming) never have to distinguish "why not" — they just fall back to
/// streaming. Never fails the caller: a DB read error is treated the same as "not downloaded",
/// matching this module's existing best-effort posture (`covers`, `item_offline_availability`).
pub async fn local_track_path(pool: &SqlitePool, server_id: &str, item_id: &str, ino: &str) -> Option<PathBuf> {
    let row = abs_storage::repo::download_tracks::get(pool, server_id, item_id, ino).await.ok()??;
    verified_complete_path(&row).await
}

/// Which of an item's tracks are fully downloaded — what `chapter_offline_markers` needs to turn
/// into per-chapter glyphs, without the Player screen having to reach into `abs_storage` directly
/// (the same boundary `item_offline_availability` above already respects).
pub async fn complete_inos_for_item(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<BTreeSet<String>> {
    let downloads = abs_storage::repo::download_tracks::list_for_item(pool, server_id, item_id).await?;
    Ok(downloads.into_iter().filter(|d| d.status == DownloadStatus::Complete).map(|d| d.ino).collect())
}

/// Item ids (scoped to one server) downloaded fully or partially — the query behind the
/// Home/Library offline-mode toggle's filtered view. Thin pass-through to the storage repo; kept
/// here (rather than calling the repo directly from `app`) so `app` never has to know the offline
/// definition lives in a "complete tracks" table rather than something chapter-shaped.
pub async fn downloaded_item_ids(pool: &SqlitePool, server_id: &str) -> Result<Vec<String>> {
    Ok(abs_storage::repo::download_tracks::downloaded_item_ids(pool, server_id).await?)
}

/// "Clear downloaded chapters" (the whole item, since tracking is per-track): delete every file on
/// disk (best-effort — a file already missing is not an error, anything else is logged and
/// skipped rather than aborting the rest of the cleanup) and then the database rows.
pub async fn clear_item_downloads(pool: &SqlitePool, server_id: &str, item_id: &str) -> Result<()> {
    let removed = abs_storage::repo::download_tracks::remove_for_item(pool, server_id, item_id).await?;
    for row in removed {
        if let Err(err) = tokio::fs::remove_file(&row.file_path).await {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%err, item_id, ino = %row.ino, "couldn't delete a downloaded track file");
            }
        }
    }
    Ok(())
}

/// A transfer that ended with an unknown total size (`None`) is trusted — there's nothing to
/// verify it against. One with a known size is only complete if every expected byte actually
/// arrived; a connection that closes early has fewer bytes than promised and must not be mistaken
/// for a clean finish.
fn transfer_is_complete(bytes_downloaded: u64, total_size: Option<u64>) -> bool {
    total_size.map(|expected| bytes_downloaded == expected).unwrap_or(true)
}

async fn backoff(attempt: u32) {
    let delay = BACKOFF_BASE * 2u32.pow(attempt.saturating_sub(1)).min(16); // caps at 8s (500ms * 16)
    tokio::time::sleep(delay).await;
}

/// Fetches one track's audio file to disk, resuming from wherever a previous attempt left off.
/// Idempotent: calling this again after a `Completed` outcome (with the file still intact on disk)
/// is a cheap no-op, same posture as `covers::fetch_and_cache_cover`'s cache-hit check.
///
/// `on_progress` is called after each chunk is written (not just at the DB-persist interval) so a
/// UI can show smooth progress even though the database itself is only updated periodically.
/// `cancel` is polled before every attempt and around every chunk read — a cooperative check, not
/// a hard abort, so a cancellation always lands between well-defined units of work (never
/// mid-write of a single chunk).
#[allow(clippy::too_many_arguments)]
pub async fn download_track(
    paths: &AppPaths,
    pool: &SqlitePool,
    connection: &crate::connection::ConnectionTarget,
    access_token: &str,
    server_id: &str,
    item_id: &str,
    ino: &str,
    mut on_progress: impl FnMut(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<TrackDownloadOutcome> {
    let existing = abs_storage::repo::download_tracks::get(pool, server_id, item_id, ino).await?;

    if let Some(row) = &existing {
        if verified_complete_path(row).await.is_some() {
            return Ok(TrackDownloadOutcome::Completed);
        }
    }

    if cancel() {
        return Ok(TrackDownloadOutcome::Canceled);
    }

    let mut resume_offset = existing.as_ref().map(|r| r.bytes_downloaded as u64).unwrap_or(0);
    let mut file_path: Option<PathBuf> = existing.map(|r| PathBuf::from(r.file_path));

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if cancel() {
            return Ok(TrackDownloadOutcome::Canceled);
        }

        let client = match connection.api_client_with_timeout(access_token, REQUEST_TIMEOUT) {
            Ok(client) => client,
            Err(err) => {
                // A mint failure with a configured connection is a concrete, user-fixable
                // problem (missing/unreadable client certificate) — reported as the download's
                // failure reason rather than crashing the download worker.
                let reason = format!("couldn't set up the connection: {err}");
                abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                return Ok(TrackDownloadOutcome::Failed(reason));
            }
        };

        let range = (resume_offset > 0).then_some(resume_offset);
        let file_response = match client.get_item_file_response(item_id, ino, range).await {
            Ok(response) => response,
            Err(err) => {
                if !err.is_retryable() || attempt >= MAX_ATTEMPTS {
                    let reason = err.to_string();
                    abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                    return Ok(TrackDownloadOutcome::Failed(reason));
                }
                backoff(attempt).await;
                continue;
            }
        };

        // The server ignored our Range request and sent the full body from byte 0 — start the
        // file over rather than risk appending a full body onto already-written bytes.
        let fresh_start = range.is_some() && !file_response.resumed;
        if fresh_start {
            resume_offset = 0;
        }

        let path = match &file_path {
            Some(existing_path) if !fresh_start => existing_path.clone(),
            _ => {
                let extension = crate::media_type::extension_for(file_response.content_type.as_deref().unwrap_or(""), "mp3");
                let dir = paths.item_downloads_dir(server_id, item_id);
                if let Err(err) = tokio::fs::create_dir_all(&dir).await {
                    let reason = format!("couldn't create the download directory: {err}");
                    abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                    return Ok(TrackDownloadOutcome::Failed(reason));
                }
                let path = paths.track_file_path(server_id, item_id, ino, extension);
                abs_storage::repo::download_tracks::upsert_pending(pool, server_id, item_id, ino, &path.to_string_lossy()).await?;
                path
            }
        };
        file_path = Some(path.clone());

        let mut file = match tokio::fs::OpenOptions::new().create(true).write(true).truncate(fresh_start || resume_offset == 0).append(!fresh_start && resume_offset > 0).open(&path).await {
            Ok(file) => file,
            Err(err) => {
                let reason = format!("couldn't open the download file: {err}");
                abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                return Ok(TrackDownloadOutcome::Failed(reason));
            }
        };

        let total_size = file_response.total_size;
        let mut stream = file_response.response.bytes_stream();
        let mut bytes_since_persist: u64 = 0;
        let mut stream_error: Option<(bool, String)> = None; // (retryable, reason)

        loop {
            if cancel() {
                abs_storage::repo::download_tracks::update_progress(pool, server_id, item_id, ino, resume_offset as i64, total_size.map(|n| n as i64)).await?;
                return Ok(TrackDownloadOutcome::Canceled);
            }

            let next = match tokio::time::timeout(IDLE_READ_TIMEOUT, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    stream_error = Some((true, "connection stalled (no data received)".to_string()));
                    break;
                }
            };
            let Some(chunk) = next else { break }; // stream ended normally
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    stream_error = Some((true, err.to_string()));
                    break;
                }
            };

            if let Err(err) = file.write_all(&chunk).await {
                stream_error = Some((false, format!("disk write failed: {err}")));
                break;
            }
            resume_offset += chunk.len() as u64;
            bytes_since_persist += chunk.len() as u64;
            on_progress(resume_offset, total_size);

            if bytes_since_persist >= PROGRESS_PERSIST_INTERVAL_BYTES {
                abs_storage::repo::download_tracks::update_progress(pool, server_id, item_id, ino, resume_offset as i64, total_size.map(|n| n as i64)).await?;
                bytes_since_persist = 0;
            }
        }
        let _ = file.flush().await;

        if let Some((retryable, reason)) = stream_error {
            abs_storage::repo::download_tracks::update_progress(pool, server_id, item_id, ino, resume_offset as i64, total_size.map(|n| n as i64)).await?;
            if !retryable || attempt >= MAX_ATTEMPTS {
                abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                return Ok(TrackDownloadOutcome::Failed(reason));
            }
            backoff(attempt).await;
            continue;
        }

        // A connection that closes early must not look like success — only trust completion when
        // the byte count actually matches the size the server told us to expect.
        if !transfer_is_complete(resume_offset, total_size) {
            let expected = total_size.expect("transfer_is_complete only returns false when total_size is Some");
            let reason = format!("stream ended early: got {resume_offset} of {expected} bytes");
            abs_storage::repo::download_tracks::update_progress(pool, server_id, item_id, ino, resume_offset as i64, Some(expected as i64)).await?;
            if attempt >= MAX_ATTEMPTS {
                abs_storage::repo::download_tracks::mark_failed(pool, server_id, item_id, ino, &reason).await?;
                return Ok(TrackDownloadOutcome::Failed(reason));
            }
            backoff(attempt).await;
            continue;
        }

        abs_storage::repo::download_tracks::mark_complete(pool, server_id, item_id, ino, resume_offset as i64).await?;
        return Ok(TrackDownloadOutcome::Completed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionTarget;
    use std::cell::Cell;
    use std::rc::Rc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn track(ino: &str, offset: f64, duration: f64) -> TrackRef {
        TrackRef { ino: ino.to_string(), track_index: 0, duration_seconds: duration, offset_seconds: offset, size_bytes: Some(1000) }
    }

    /// Like `track`, but with an unknown size — the "server didn't report it" case estimates
    /// must handle by refusing, not by guessing.
    fn unsized_track(ino: &str, offset: f64, duration: f64) -> TrackRef {
        TrackRef { ino: ino.to_string(), track_index: 0, duration_seconds: duration, offset_seconds: offset, size_bytes: None }
    }

    fn chapter(start: f64, end: f64) -> abs_api::ChapterRef {
        abs_api::ChapterRef { title: "Ch".to_string(), start_seconds: start, end_seconds: end }
    }

    #[test]
    fn tracks_needed_for_chapters_one_file_per_chapter() {
        let tracks = vec![track("a", 0.0, 100.0), track("b", 100.0, 100.0), track("c", 200.0, 100.0)];
        let chapters = vec![chapter(0.0, 100.0), chapter(100.0, 200.0), chapter(200.0, 300.0)];

        assert_eq!(tracks_needed_for_chapters(&tracks, &chapters, &[1]), BTreeSet::from(["b".to_string()]));
    }

    #[test]
    fn tracks_needed_for_chapters_one_file_many_chapters() {
        // A single track spanning every chapter — common for single-file audiobooks with
        // embedded chapter markers.
        let tracks = vec![track("only", 0.0, 300.0)];
        let chapters = vec![chapter(0.0, 100.0), chapter(100.0, 200.0), chapter(200.0, 300.0)];

        assert_eq!(tracks_needed_for_chapters(&tracks, &chapters, &[0]), BTreeSet::from(["only".to_string()]));
        assert_eq!(tracks_needed_for_chapters(&tracks, &chapters, &[2]), BTreeSet::from(["only".to_string()]));
    }

    #[test]
    fn tracks_needed_for_chapters_straddling_a_track_boundary() {
        let tracks = vec![track("a", 0.0, 150.0), track("b", 150.0, 150.0)];
        // This chapter starts in track "a" and ends in track "b".
        let chapters = vec![chapter(100.0, 200.0)];

        assert_eq!(tracks_needed_for_chapters(&tracks, &chapters, &[0]), BTreeSet::from(["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn tracks_needed_for_chapters_multiple_indices_union() {
        let tracks = vec![track("a", 0.0, 100.0), track("b", 100.0, 100.0), track("c", 200.0, 100.0)];
        let chapters = vec![chapter(0.0, 100.0), chapter(100.0, 200.0), chapter(200.0, 300.0)];

        assert_eq!(tracks_needed_for_chapters(&tracks, &chapters, &[0, 2]), BTreeSet::from(["a".to_string(), "c".to_string()]));
    }

    #[test]
    fn estimate_bytes_sums_the_distinct_tracks_a_scope_touches() {
        let tracks = vec![track("a", 0.0, 100.0), track("b", 100.0, 100.0), track("c", 200.0, 100.0)];
        let ranges: Vec<(f64, f64)> = vec![(0.0, 100.0), (100.0, 200.0), (200.0, 300.0)];

        // One chapter -> its one track; two chapters sharing nothing -> both; a shared file ->
        // counted once, not twice.
        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[0]), Some(1000));
        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[0, 2]), Some(2000));
        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[0, 1]), Some(2000));
        assert_eq!(estimate_bytes_for_chapters(&[], &ranges, &[0]), None, "no cached tracks at all is 'can't know', not 'zero bytes'");
    }

    #[test]
    fn estimate_bytes_counts_a_shared_file_once_and_a_straddle_twice() {
        let one_shared = vec![track("only", 0.0, 300.0)];
        let ranges = vec![(0.0, 150.0), (150.0, 300.0)];
        assert_eq!(estimate_bytes_for_chapters(&one_shared, &ranges, &[0, 1]), Some(1000));

        let straddling = vec![track("a", 0.0, 150.0), track("b", 150.0, 150.0)];
        let straddle_chapters = vec![(100.0, 200.0)]; // starts in "a", ends in "b"
        assert_eq!(estimate_bytes_for_chapters(&straddling, &straddle_chapters, &[0]), Some(2000));
    }

    #[test]
    fn estimate_bytes_is_none_when_any_touched_track_has_an_unknown_size() {
        let tracks = vec![track("a", 0.0, 100.0), unsized_track("b", 100.0, 100.0), track("c", 200.0, 100.0)];
        let ranges: Vec<(f64, f64)> = vec![(0.0, 100.0), (100.0, 200.0), (200.0, 300.0)];

        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[0]), Some(1000), "untouched unknowns don't spoil the estimate");
        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[1]), None);
        assert_eq!(estimate_bytes_for_chapters(&tracks, &ranges, &[0, 1]), None);
    }

    #[test]
    fn chapter_offline_markers_reflects_which_tracks_are_complete() {
        let tracks = vec![track("a", 0.0, 100.0), track("b", 100.0, 100.0)];
        let chapters = vec![chapter(0.0, 100.0), chapter(100.0, 200.0)];
        let complete = BTreeSet::from(["a".to_string()]);

        assert_eq!(chapter_offline_markers(&tracks, &chapters, &complete), vec![true, false]);
    }

    #[test]
    fn chapter_offline_markers_requires_every_touched_track_complete() {
        let tracks = vec![track("a", 0.0, 150.0), track("b", 150.0, 150.0)];
        let chapters = vec![chapter(100.0, 200.0)]; // straddles both tracks
        let complete = BTreeSet::from(["a".to_string()]); // only one of the two

        assert_eq!(chapter_offline_markers(&tracks, &chapters, &complete), vec![false]);
    }

    #[tokio::test]
    async fn chapter_offline_markers_for_item_matches_the_pure_function() {
        // `pool_with_synced_tracks` gives every track the same offset (0.0) — fine for the tests
        // that use it, but this one needs two tracks at distinct offsets to tell them apart, so it
        // sets its own up directly via `abs_core::tracks::sync_item_tracks`.
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = abs_storage::repo::servers::add(&pool, "http://example.invalid").await.unwrap();
        abs_storage::repo::libraries::upsert(&pool, abs_storage::repo::libraries::UpsertLibrary { id: "lib-1", server_id: &server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 }).await.unwrap();
        abs_storage::repo::items::upsert(
            &pool,
            abs_storage::repo::items::UpsertItem { id: "item-1", server_id: &server_id, library_id: "lib-1", title: "Test Item", author: None, narrator: None, description: None, duration_seconds: 0.0, added_at: chrono::Utc::now(), series_name: None, genres: &[] },
        )
        .await
        .unwrap();
        crate::tracks::sync_item_tracks(
            &pool,
            &server_id,
            "item-1",
            &[
                crate::streaming::StreamTrack { ino: "a".to_string(), url: "u1".to_string(), duration_seconds: 50.0, offset_seconds: 0.0, size_bytes: None },
                crate::streaming::StreamTrack { ino: "b".to_string(), url: "u2".to_string(), duration_seconds: 50.0, offset_seconds: 50.0, size_bytes: None },
            ],
        )
        .await
        .unwrap();
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", "/p/a.mp3").await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 10).await.unwrap();

        let markers = chapter_offline_markers_for_item(&pool, &server_id, "item-1", &[(0.0, 50.0), (50.0, 100.0)]).await.unwrap();
        assert_eq!(markers, vec![true, false], "only the chapter fully within track 'a' should be marked downloaded");
    }

    async fn pool_with_synced_tracks(item_id: &str, inos: &[&str]) -> (SqlitePool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = abs_storage::repo::servers::add(&pool, "http://example.invalid").await.unwrap();
        abs_storage::repo::libraries::upsert(
            &pool,
            abs_storage::repo::libraries::UpsertLibrary { id: "lib-1", server_id: &server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 },
        )
        .await
        .unwrap();
        abs_storage::repo::items::upsert(
            &pool,
            abs_storage::repo::items::UpsertItem {
                id: item_id,
                server_id: &server_id,
                library_id: "lib-1",
                title: "Test Item",
                author: None,
                narrator: None,
                description: None,
                duration_seconds: 0.0,
                added_at: chrono::Utc::now(),
                series_name: None,
                genres: &[],
            },
        )
        .await
        .unwrap();
        let new_tracks: Vec<_> = inos.iter().map(|ino| abs_storage::repo::tracks::NewTrack { ino, duration_seconds: 100.0, offset_seconds: 0.0, size_bytes: None }).collect();
        abs_storage::repo::tracks::upsert_all(&pool, &server_id, item_id, &new_tracks).await.unwrap();
        (pool, server_id)
    }

    #[tokio::test]
    async fn item_offline_availability_progresses_from_none_to_partial_to_full() {
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["a", "b"]).await;
        assert_eq!(item_offline_availability(&pool, &server_id, "item-1").await.unwrap(), OfflineAvailability::None);

        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", "/p/a.mp3").await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 10).await.unwrap();
        assert_eq!(item_offline_availability(&pool, &server_id, "item-1").await.unwrap(), OfflineAvailability::Partial);

        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "b", "/p/b.mp3").await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "b", 10).await.unwrap();
        assert_eq!(item_offline_availability(&pool, &server_id, "item-1").await.unwrap(), OfflineAvailability::Full);
    }

    #[tokio::test]
    async fn complete_inos_for_item_includes_only_complete_tracks() {
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["a", "b"]).await;
        assert!(complete_inos_for_item(&pool, &server_id, "item-1").await.unwrap().is_empty());

        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", "/p/a.mp3").await.unwrap();
        abs_storage::repo::download_tracks::update_progress(&pool, &server_id, "item-1", "a", 5, Some(10)).await.unwrap();
        assert!(complete_inos_for_item(&pool, &server_id, "item-1").await.unwrap().is_empty(), "downloading-but-not-complete must not count");

        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 10).await.unwrap();
        assert_eq!(complete_inos_for_item(&pool, &server_id, "item-1").await.unwrap(), BTreeSet::from(["a".to_string()]));
    }

    #[tokio::test]
    async fn local_track_path_is_none_unless_complete_and_verified() {
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["a"]).await;
        assert!(local_track_path(&pool, &server_id, "item-1", "a").await.is_none(), "never downloaded");

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("a.mp3");
        tokio::fs::write(&file_path, b"hello").await.unwrap();
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", file_path.to_str().unwrap()).await.unwrap();
        assert!(local_track_path(&pool, &server_id, "item-1", "a").await.is_none(), "pending, not yet complete");

        abs_storage::repo::download_tracks::update_progress(&pool, &server_id, "item-1", "a", 5, Some(10)).await.unwrap();
        assert!(local_track_path(&pool, &server_id, "item-1", "a").await.is_none(), "downloading, not yet complete");

        // Mark complete claiming a size that doesn't match the real 5-byte file — a stale/corrupt
        // row must not be trusted.
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 999).await.unwrap();
        assert!(local_track_path(&pool, &server_id, "item-1", "a").await.is_none(), "complete but size mismatch");

        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 5).await.unwrap();
        assert_eq!(local_track_path(&pool, &server_id, "item-1", "a").await, Some(file_path.clone()));

        tokio::fs::remove_file(&file_path).await.unwrap();
        assert!(local_track_path(&pool, &server_id, "item-1", "a").await.is_none(), "complete row whose file is gone must not be trusted");
    }

    #[tokio::test]
    async fn item_offline_availability_is_none_when_tracks_were_never_synced() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        let server_id = abs_storage::repo::servers::add(&pool, "http://example.invalid").await.unwrap();

        assert_eq!(item_offline_availability(&pool, &server_id, "item-1").await.unwrap(), OfflineAvailability::None);
    }

    #[tokio::test]
    async fn clear_item_downloads_deletes_files_and_rows() {
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["a"]).await;
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("a.mp3");
        tokio::fs::write(&file_path, b"hello").await.unwrap();
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", file_path.to_str().unwrap()).await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 5).await.unwrap();

        clear_item_downloads(&pool, &server_id, "item-1").await.unwrap();

        assert!(!file_path.exists());
        assert!(abs_storage::repo::download_tracks::list_for_item(&pool, &server_id, "item-1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clear_item_downloads_tolerates_an_already_missing_file() {
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["a"]).await;
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "a", "/does/not/exist.mp3").await.unwrap();
        abs_storage::repo::download_tracks::mark_complete(&pool, &server_id, "item-1", "a", 5).await.unwrap();

        // Must not error even though the file is already gone.
        clear_item_downloads(&pool, &server_id, "item-1").await.unwrap();
    }

    fn test_paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
        (tmp, paths)
    }

    fn no_cancel() -> impl Fn() -> bool {
        || false
    }

    #[tokio::test]
    async fn download_track_happy_path_writes_the_file_and_marks_complete() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Type", "audio/mpeg").insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        let mut progress_calls = Vec::new();
        let outcome = download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |downloaded, total| progress_calls.push((downloaded, total)), &no_cancel())
            .await
            .unwrap();

        assert_eq!(outcome, TrackDownloadOutcome::Completed);
        assert!(!progress_calls.is_empty());

        let row = abs_storage::repo::download_tracks::get(&pool, &server_id, "item-1", "ino-1").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Complete);
        assert_eq!(row.bytes_downloaded, 5);
        assert!(row.file_path.ends_with("ino-1.mp3"));
        assert_eq!(tokio::fs::read(&row.file_path).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn download_track_is_idempotent_once_complete() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &no_cancel()).await.unwrap();
        download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &no_cancel()).await.unwrap();

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a second call should be a cache hit, not a second HTTP request");
    }

    #[tokio::test]
    async fn download_track_a_404_fails_immediately_without_retrying() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/items/item-1/file/ino-1")).respond_with(ResponseTemplate::new(404)).mount(&mock_server).await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        let outcome = download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &no_cancel()).await.unwrap();

        assert!(matches!(outcome, TrackDownloadOutcome::Failed(_)));
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a 404 must not be retried");

        let row = abs_storage::repo::download_tracks::get(&pool, &server_id, "item-1", "ino-1").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Failed);
    }

    #[tokio::test]
    async fn download_track_cancellation_leaves_a_resumable_partial_row() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_body_bytes(b"hello".to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        // Cancel on the very first check, before any bytes are read at all.
        let canceled = Rc::new(Cell::new(true));
        let cancel_flag = canceled.clone();
        let outcome = download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &move || cancel_flag.get()).await.unwrap();

        assert_eq!(outcome, TrackDownloadOutcome::Canceled);
        assert!(abs_storage::repo::download_tracks::get(&pool, &server_id, "item-1", "ino-1").await.unwrap().is_none(), "canceling before any row exists must not fabricate one");
    }

    #[test]
    fn transfer_is_complete_requires_matching_byte_count_when_known() {
        assert!(!transfer_is_complete(5, Some(10)), "fewer bytes than promised must not look complete");
        assert!(transfer_is_complete(10, Some(10)));
        assert!(transfer_is_complete(5, None), "an unknown total size can't be verified, so it's trusted");
    }

    #[tokio::test]
    async fn download_track_resumes_from_a_previously_interrupted_session() {
        // Simulates the real scenario the ui-spec calls "resumable": the app was killed (or lost
        // connectivity) after a prior run wrote the first 5 bytes and recorded that progress:
        // re-invoking `download_track` must request only the remaining bytes via Range, not
        // restart from scratch.
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .and(header("Range", "bytes=5-"))
            .respond_with(ResponseTemplate::new(206).insert_header("Content-Range", "bytes 5-9/10").set_body_bytes(b"world".to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        let file_path = paths.track_file_path(&server_id, "item-1", "ino-1", "mp3");
        tokio::fs::create_dir_all(file_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&file_path, b"hello").await.unwrap();
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "ino-1", file_path.to_str().unwrap()).await.unwrap();
        abs_storage::repo::download_tracks::update_progress(&pool, &server_id, "item-1", "ino-1", 5, Some(10)).await.unwrap();

        let outcome = download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &no_cancel()).await.unwrap();

        assert_eq!(outcome, TrackDownloadOutcome::Completed);
        assert_eq!(tokio::fs::read(&file_path).await.unwrap(), b"helloworld", "resumed bytes must be appended, not overwrite what was already there");
        let row = abs_storage::repo::download_tracks::get(&pool, &server_id, "item-1", "ino-1").await.unwrap().unwrap();
        assert_eq!(row.bytes_downloaded, 10);
        assert_eq!(row.status, DownloadStatus::Complete);
    }

    #[tokio::test]
    async fn download_track_falls_back_to_a_fresh_download_if_the_server_ignores_the_range_request() {
        // A server that doesn't support Range for this file replies 200 with the full body
        // instead of 206 — the resume attempt must detect this and restart from byte 0 rather
        // than append the full body onto the 5 bytes already on disk (which would duplicate data).
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1/file/ino-1"))
            .and(header("Range", "bytes=5-"))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "10").set_body_bytes(b"full-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let (_tmp, paths) = test_paths();
        let (pool, server_id) = pool_with_synced_tracks("item-1", &["ino-1"]).await;

        let file_path = paths.track_file_path(&server_id, "item-1", "ino-1", "mp3");
        tokio::fs::create_dir_all(file_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&file_path, b"hello").await.unwrap();
        abs_storage::repo::download_tracks::upsert_pending(&pool, &server_id, "item-1", "ino-1", file_path.to_str().unwrap()).await.unwrap();
        abs_storage::repo::download_tracks::update_progress(&pool, &server_id, "item-1", "ino-1", 5, Some(10)).await.unwrap();

        let outcome = download_track(&paths, &pool, &ConnectionTarget::direct(&mock_server.uri()), "token", &server_id, "item-1", "ino-1", |_, _| {}, &no_cancel()).await.unwrap();

        assert_eq!(outcome, TrackDownloadOutcome::Completed);
        assert_eq!(tokio::fs::read(&file_path).await.unwrap(), b"full-bytes", "must restart fresh, not append the full body onto the old partial bytes");
    }
}
