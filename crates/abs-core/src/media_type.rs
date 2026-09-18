//! Content-type-to-file-extension mapping, shared by everything that caches server-fetched bytes
//! under a real filename (`covers::fetch_and_cache_cover`, `download_tracks::download_track`).
//! Audiobookshelf serves both covers and audio tracks in whatever format the source file actually
//! is — never assume one extension.

/// `default` is returned for an unrecognized or absent content-type — callers pick a sensible
/// fallback for their own media kind (`"jpg"` for covers, `"mp3"` for audio tracks), since there's
/// no single reasonable default across both.
pub fn extension_for(content_type: &str, default: &'static str) -> &'static str {
    match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/jpeg" => "jpg",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/m4b" | "audio/x-m4b" => "m4b",
        "audio/ogg" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/x-wav" | "audio/wav" => "wav",
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_known_image_formats() {
        assert_eq!(extension_for("image/webp", "jpg"), "webp");
        assert_eq!(extension_for("image/png", "jpg"), "png");
        assert_eq!(extension_for("image/jpeg", "jpg"), "jpg");
        assert_eq!(extension_for("image/webp; charset=binary", "jpg"), "webp");
    }

    #[test]
    fn covers_the_known_audio_formats() {
        assert_eq!(extension_for("audio/mpeg", "mp3"), "mp3");
        assert_eq!(extension_for("audio/mp4", "mp3"), "m4a");
        assert_eq!(extension_for("audio/x-m4a", "mp3"), "m4a");
        assert_eq!(extension_for("audio/m4b", "mp3"), "m4b");
        assert_eq!(extension_for("audio/x-flac", "mp3"), "flac");
    }

    #[test]
    fn unknown_or_absent_content_type_falls_back_to_the_given_default() {
        assert_eq!(extension_for("application/octet-stream", "jpg"), "jpg");
        assert_eq!(extension_for("", "mp3"), "mp3");
    }
}
