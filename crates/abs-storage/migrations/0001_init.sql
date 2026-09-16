-- Servers: connection settings for a configured Audiobookshelf instance.
-- Corresponds to Settings > Servers and the Connection screen in docs/design/ui-spec.md.
CREATE TABLE servers (
    id                      TEXT PRIMARY KEY NOT NULL, -- uuid
    url                     TEXT NOT NULL,
    custom_headers_json     TEXT NOT NULL DEFAULT '{}',
    disable_ssl_verify      INTEGER NOT NULL DEFAULT 0 CHECK (disable_ssl_verify IN (0, 1)),
    client_cert_path        TEXT,
    local_network_address   TEXT,
    user_agent              TEXT,
    created_at              TEXT NOT NULL -- RFC3339
);

-- Accounts: a logged-in user on a given server. Exactly one account per server may be active
-- (the account switch-server flow deactivates the previous one before activating the next).
CREATE TABLE accounts (
    id              TEXT PRIMARY KEY NOT NULL, -- uuid
    server_id       TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    username        TEXT NOT NULL,
    token           TEXT NOT NULL,
    is_active       INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at      TEXT NOT NULL,
    UNIQUE (server_id, username)
);

-- Partial-unique-index equivalent: SQLite supports partial indexes directly.
CREATE UNIQUE INDEX accounts_one_active_idx ON accounts(is_active) WHERE is_active = 1;

-- Libraries: cached from the server's /api/libraries, scoped per server.
CREATE TABLE libraries (
    id              TEXT NOT NULL, -- server-assigned uuid
    server_id       TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    media_type      TEXT NOT NULL, -- 'book' | 'podcast'
    icon            TEXT,
    display_order   INTEGER NOT NULL DEFAULT 1,
    synced_at       TEXT NOT NULL,
    PRIMARY KEY (server_id, id)
);

-- Items: cached library item metadata (audiobooks or podcasts).
CREATE TABLE items (
    id              TEXT NOT NULL, -- server-assigned uuid
    server_id       TEXT NOT NULL,
    library_id      TEXT NOT NULL,
    title           TEXT NOT NULL,
    author          TEXT,
    narrator        TEXT,
    description     TEXT,
    cover_cache_path TEXT,
    duration_seconds REAL NOT NULL DEFAULT 0,
    added_at        TEXT NOT NULL,
    synced_at       TEXT NOT NULL,
    PRIMARY KEY (server_id, id),
    FOREIGN KEY (server_id, library_id) REFERENCES libraries(server_id, id) ON DELETE CASCADE
);

CREATE INDEX items_library_idx ON items(server_id, library_id);

-- Chapters: ordered chapter list for an item, cached from the server.
CREATE TABLE chapters (
    server_id       TEXT NOT NULL,
    item_id         TEXT NOT NULL,
    chapter_index   INTEGER NOT NULL, -- 0-based position within the item
    title           TEXT NOT NULL,
    start_seconds   REAL NOT NULL,
    end_seconds     REAL NOT NULL,
    PRIMARY KEY (server_id, item_id, chapter_index),
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

-- Progress: local + last-synced playback position for an item, per account.
CREATE TABLE progress (
    account_id          TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    server_id           TEXT NOT NULL,
    item_id             TEXT NOT NULL,
    current_time_seconds REAL NOT NULL DEFAULT 0,
    is_finished         INTEGER NOT NULL DEFAULT 0 CHECK (is_finished IN (0, 1)),
    updated_at          TEXT NOT NULL,
    PRIMARY KEY (account_id, server_id, item_id),
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

-- Downloads: one row per downloaded chapter file, so partial-download scopes (current / next N /
-- remaining / entire book) and "clear downloaded chapters" map directly onto rows here.
CREATE TABLE downloads (
    server_id       TEXT NOT NULL,
    item_id         TEXT NOT NULL,
    chapter_index   INTEGER NOT NULL,
    file_path       TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL DEFAULT 0,
    downloaded_at   TEXT NOT NULL,
    PRIMARY KEY (server_id, item_id, chapter_index),
    FOREIGN KEY (server_id, item_id, chapter_index)
        REFERENCES chapters(server_id, item_id, chapter_index) ON DELETE CASCADE
);

-- Settings: generic key/value store for app-wide preferences (playback defaults, appearance,
-- library-browse view options, etc.) — see docs/architecture note on keeping structured state in
-- one place instead of a separate config file.
CREATE TABLE settings (
    key     TEXT PRIMARY KEY NOT NULL,
    value   TEXT NOT NULL
);
