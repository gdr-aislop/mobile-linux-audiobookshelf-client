-- Replaces the original `downloads` table (one row per *chapter*, each with its own file) with a
-- per-*track* model. The real Audiobookshelf server only ever serves whole audio files
-- (`GET /api/items/:id/file/:ino`, no per-chapter endpoint), and a single file commonly contains
-- many chapters (embedded chapter markers) — the old table's assumption of one file per chapter
-- doesn't match reality for those items. Nothing has ever written a real row to it (no download
-- pipeline existed yet), so dropping it outright is safe.
DROP TABLE downloads;

-- Cached per-item track metadata, mirroring `abs_core::streaming::StreamTrack` (the same shape
-- already fetched via `resolve_stream_target` for playback) so it's available offline without a
-- network round trip. `ino` is the server's own file identifier and the natural external key;
-- `track_index` just preserves ordering for display/iteration.
CREATE TABLE tracks (
    server_id        TEXT NOT NULL,
    item_id          TEXT NOT NULL,
    ino              TEXT NOT NULL,
    track_index      INTEGER NOT NULL,
    duration_seconds REAL NOT NULL,
    offset_seconds   REAL NOT NULL,
    PRIMARY KEY (server_id, item_id, ino),
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

-- One row per track download attempt/result — the real unit of network I/O and disk file. Tracks
-- `bytes_downloaded` (not just complete-or-not) so an interrupted transfer can resume with an
-- HTTP `Range` request instead of restarting from scratch, per docs/design/ui-spec.md's "Downloads
-- are resumable" requirement.
CREATE TABLE download_tracks (
    server_id            TEXT NOT NULL,
    item_id              TEXT NOT NULL,
    ino                  TEXT NOT NULL,
    file_path            TEXT NOT NULL,
    expected_size_bytes  INTEGER,
    bytes_downloaded     INTEGER NOT NULL DEFAULT 0,
    status               TEXT NOT NULL DEFAULT 'pending', -- pending | downloading | complete | failed
    error_reason         TEXT,
    updated_at           TEXT NOT NULL,
    PRIMARY KEY (server_id, item_id, ino),
    FOREIGN KEY (server_id, item_id, ino) REFERENCES tracks(server_id, item_id, ino) ON DELETE CASCADE
);
