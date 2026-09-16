//! Resolves a playable URL for an item's audio, for the Player screen. `abs-api` has no coverage
//! for `/api/items/*` at all (see `third_party/audiobookshelf-openapi/README.md`'s "Known gaps"),
//! so `abs_api::Client::get_item_playback_info` is a hand-written extension; this module is the
//! one place that turns its result into something `abs-player` can actually load.

use crate::error::{CoreError, Result};

pub struct StreamTarget {
    pub url: String,
    pub duration_seconds: f64,
    /// A value greater than 1 means this item has more than one audio file (a multi-track
    /// book/podcast). Full multi-track sequencing isn't implemented — only the first file is ever
    /// played — so callers use this to show a caveat rather than silently truncating the book.
    pub track_count: usize,
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

    let first = info
        .audio_files
        .first()
        .ok_or_else(|| CoreError::UnexpectedResponse(format!("item {item_id} has no audio files")))?;

    Ok(StreamTarget {
        url: format!("{server_url}/api/items/{item_id}/file/{}?token={access_token}", first.ino),
        duration_seconds: first.duration_seconds,
        track_count: info.audio_files.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
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

        assert_eq!(
            target.url,
            format!("{}/api/items/item-1/file/12345?token=test-token", mock_server.uri())
        );
        assert_eq!(target.duration_seconds, 3600.0);
        assert_eq!(target.track_count, 1);
    }

    #[tokio::test]
    async fn resolve_stream_target_reports_a_multi_track_count() {
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

        assert_eq!(target.track_count, 2, "should report every audio file even though only the first is playable");
        assert!(target.url.contains("/file/111"), "should play the first file: {}", target.url);
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
}
