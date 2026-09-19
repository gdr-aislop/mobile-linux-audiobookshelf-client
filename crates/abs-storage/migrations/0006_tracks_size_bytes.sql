-- Adds each cached track's file size in bytes (as the server reports it in the item's audio-file
-- metadata), so download scope rows can show honest "≈NNN MB" size estimates offline — the same
-- data `download_tracks.expected_size_bytes` records once a download starts, but known before any
-- download begins. Nullable: items synced before this column existed simply have no estimate
-- until their tracks are re-synced, and a server may omit the size outright.
ALTER TABLE tracks ADD COLUMN size_bytes INTEGER;
