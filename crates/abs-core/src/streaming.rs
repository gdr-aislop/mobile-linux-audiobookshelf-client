//! Resolves playable URLs for an item's audio, for the Player screen. `abs-api` has no coverage
//! for `/api/items/*` at all (see `third_party/audiobookshelf-openapi/README.md`'s "Known gaps"),
//! so `abs_api::Client::get_item_playback_info` is a hand-written extension; this module is the
//! one place that turns its result into something `abs-player` can actually load. Items split
//! across multiple audio files get one `StreamTrack` per file, each with its own URL — the caller
//! plays them in sequence.

use crate::error::{CoreError, Result};

pub struct StreamTarget {
    /// One entry per audio file, in book order. A book split across multiple files is played by
    /// loading these one after another — see `StreamTrack::offset_seconds` for how callers map
    /// book-level positions onto individual files.
    pub tracks: Vec<StreamTrack>,
    /// The book-level duration: the sum of every track's duration, not any single file's.
    pub duration_seconds: f64,
    /// The item's chapters, if any — comes free in the same `GET /api/items/:id` response used to
    /// resolve the audio files, so no second network call is needed to get this. Chapter
    /// positions are book-level, spanning across file boundaries.
    pub chapters: Vec<abs_api::ChapterRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamTrack {
    pub ino: String,
    pub url: String,
    pub duration_seconds: f64,
    /// Where this file starts within the whole book, computed from the server-reported durations
    /// of the files before it. Progress and chapters are defined book-level, so everything
    /// user-facing positions itself through this.
    pub offset_seconds: f64,
}

/// Builds an authenticated `abs_api::Client` from `server_url`/`access_token` itself — the same
/// boundary `abs_core::sync::sync_all` already draws — so callers (the `app` crate) never
/// construct one themselves.
pub async fn resolve_stream_target(server_url: &str, access_token: &str, item_id: &str) -> Result<StreamTarget> {
    let api = abs_api::Client::with_bearer_token(server_url, access_token)
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    let info = api
        .get_item_playback_info(item_id)
        .await
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;

    if info.audio_files.is_empty() {
        return Err(CoreError::UnexpectedResponse(format!("item {item_id} has no audio files")));
    }

    let mut tracks = Vec::with_capacity(info.audio_files.len());
    let mut offset_seconds = 0.0;
    for file in &info.audio_files {
        tracks.push(StreamTrack {
            ino: file.ino.clone(),
            url: track_url(server_url, item_id, &file.ino, access_token),
            duration_seconds: file.duration_seconds,
            offset_seconds,
        });
        offset_seconds += file.duration_seconds;
    }

    Ok(StreamTarget { tracks, duration_seconds: offset_seconds, chapters: info.chapters })
}

/// Finds which track a book-level position falls into, and how far into that track it is.
/// `rposition` (not `position`) matters at an exact track boundary: a position that lands exactly
/// on the start of a later track must resolve to that later track, not linger on the end of the
/// previous one. Shared by playback (mapping the resume position and seek targets onto the
/// currently-loaded track) and downloads (mapping a chapter's `[start, end)` range onto the set of
/// tracks it touches) — one definition, tested once.
pub fn locate_track(tracks: &[StreamTrack], book_seconds: f64) -> (usize, f64) {
    let index = tracks.iter().rposition(|t| t.offset_seconds <= book_seconds + 1e-6).unwrap_or(0);
    (index, (book_seconds - tracks[index].offset_seconds).max(0.0))
}

/// The authenticated stream URL for a single audio file. Exposed separately because track URLs
/// are **baked into a loaded pipeline** for as long as that file plays — a caller holding a
/// [`StreamTarget`] across a long session (multi-file books advance hours later) rebuilds the
/// URL with a current token via this, instead of replaying the stale one baked at resolve time.
pub fn track_url(server_url: &str, item_id: &str, ino: &str, access_token: &str) -> String {
    format!("{server_url}/api/items/{item_id}/file/{ino}?token={access_token}")
}

/// Pushes local playback progress up to the server, so it shows up in the official apps and
/// survives a fresh install — not just recorded in this client's own local `progress` table.
/// Callers are expected to treat a failure here as non-fatal: the local write (the source of
/// truth for this client's own "Continue Listening") already happened by the time this runs, and
/// a transient network failure syncing it up shouldn't be surfaced as a playback error.
pub async fn sync_progress_to_server(
    server_url: &str,
    access_token: &str,
    item_id: &str,
    current_time_seconds: f64,
    duration_seconds: f64,
    is_finished: bool,
) -> Result<()> {
    let api = abs_api::Client::with_bearer_token(server_url, access_token)
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))?;
    api.update_media_progress(item_id, current_time_seconds, duration_seconds, is_finished)
        .await
        .map_err(|e| CoreError::UnexpectedResponse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolve_stream_target_builds_the_expected_url() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "12345", "duration": 3600.0 }] }
            })))
            .mount(&mock_server)
            .await;

        let target = resolve_stream_target(&mock_server.uri(), "test-token", "item-1").await.unwrap();

        assert_eq!(target.tracks.len(), 1);
        assert_eq!(
            target.tracks[0].url,
            format!("{}/api/items/item-1/file/12345?token=test-token", mock_server.uri())
        );
        assert_eq!(target.tracks[0].duration_seconds, 3600.0);
        assert_eq!(target.tracks[0].offset_seconds, 0.0);
        assert_eq!(target.duration_seconds, 3600.0);
        assert!(target.chapters.is_empty());
    }

    #[tokio::test]
    async fn resolve_stream_target_includes_chapters() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [{ "ino": "12345", "duration": 3600.0 }],
                    "chapters": [
                        { "id": 0, "start": 0.0, "end": 1800.0, "title": "Part One" },
                        { "id": 1, "start": 1800.0, "end": 3600.0, "title": "Part Two" },
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let target = resolve_stream_target(&mock_server.uri(), "test-token", "item-1").await.unwrap();

        assert_eq!(target.chapters.len(), 2);
        assert_eq!(target.chapters[0].title, "Part One");
        assert_eq!(target.chapters[1].start_seconds, 1800.0);
    }

    #[tokio::test]
    async fn resolve_stream_target_lists_every_track_with_its_offset() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": {
                    "audioFiles": [
                        { "ino": "111", "duration": 1800.0 },
                        { "ino": "222", "duration": 1800.0 },
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let target = resolve_stream_target(&mock_server.uri(), "test-token", "item-1").await.unwrap();

        assert_eq!(target.tracks.len(), 2, "should list every audio file, not just the first");
        assert!(target.tracks[0].url.contains("/file/111"), "first track's URL: {}", target.tracks[0].url);
        assert!(target.tracks[1].url.contains("/file/222"), "second track's URL: {}", target.tracks[1].url);
        assert_eq!(target.tracks[0].offset_seconds, 0.0);
        assert_eq!(target.tracks[1].offset_seconds, 1800.0, "the second track starts where the first ends");
        assert_eq!(target.duration_seconds, 3600.0, "book-level duration spans every track");
    }

    #[tokio::test]
    async fn resolve_stream_target_with_no_audio_files_is_an_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [] }
            })))
            .mount(&mock_server)
            .await;

        let result = resolve_stream_target(&mock_server.uri(), "test-token", "item-1").await;
        assert!(result.is_err(), "an item with no audio files at all can't be played");
    }

    #[tokio::test]
    async fn resolve_stream_target_propagates_server_errors() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/items/item-1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let result = resolve_stream_target(&mock_server.uri(), "test-token", "item-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sync_progress_to_server_sends_the_expected_request() {
        let mock_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/me/progress/item-1"))
            .and(body_partial_json(serde_json::json!({
                "currentTime": 42.5,
                "duration": 100.0,
                "isFinished": false,
            })))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        sync_progress_to_server(&mock_server.uri(), "test-token", "item-1", 42.5, 100.0, false).await.unwrap();
    }

    fn two_tracks() -> Vec<StreamTrack> {
        vec![
            StreamTrack { ino: "1".into(), url: "u1".into(), duration_seconds: 1800.0, offset_seconds: 0.0 },
            StreamTrack { ino: "2".into(), url: "u2".into(), duration_seconds: 1800.0, offset_seconds: 1800.0 },
        ]
    }

    #[test]
    fn locate_track_finds_the_containing_track() {
        assert_eq!(locate_track(&two_tracks(), 900.0), (0, 900.0));
        assert_eq!(locate_track(&two_tracks(), 2000.0), (1, 200.0));
    }

    #[test]
    fn locate_track_at_an_exact_boundary_resolves_to_the_later_track() {
        assert_eq!(locate_track(&two_tracks(), 1800.0), (1, 0.0));
    }

    #[test]
    fn locate_track_before_the_first_track_clamps_to_it() {
        assert_eq!(locate_track(&two_tracks(), -5.0), (0, 0.0));
    }

    #[tokio::test]
    async fn sync_progress_to_server_propagates_server_errors() {
        let mock_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/api/me/progress/item-1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let result = sync_progress_to_server(&mock_server.uri(), "test-token", "item-1", 42.5, 100.0, false).await;
        assert!(result.is_err());
    }
}
