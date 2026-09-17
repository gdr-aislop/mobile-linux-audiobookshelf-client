-- Bookmarks: a local-only marker at a position in an item ("Add bookmark" in the player screen's
-- header menu). No server sync and no list/browse UI are specified anywhere in
-- docs/design/ui-spec.md, so this table exists purely to record the write.
CREATE TABLE bookmarks (
    id              TEXT PRIMARY KEY NOT NULL, -- uuid
    account_id      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    server_id       TEXT NOT NULL,
    item_id         TEXT NOT NULL,
    position_seconds REAL NOT NULL,
    created_at      TEXT NOT NULL,
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

CREATE INDEX bookmarks_item_idx ON bookmarks(server_id, item_id);
