-- Per-series book membership, cached from `GET /api/libraries/{id}/series` — the only place
-- Audiobookshelf's API exposes a book's real position ("sequence") within a series. Replaced
-- wholesale per library on every fetch (see repo::series_books::replace_all_for_library), same
-- "server sends the full list every time" reasoning chapters/tracks already use.
CREATE TABLE series_books (
    server_id   TEXT NOT NULL,
    library_id  TEXT NOT NULL,
    series_id   TEXT NOT NULL,
    series_name TEXT NOT NULL,
    item_id     TEXT NOT NULL,
    sequence    TEXT,
    PRIMARY KEY (server_id, series_id, item_id),
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

CREATE INDEX series_books_item_idx ON series_books(server_id, item_id);
CREATE INDEX series_books_library_idx ON series_books(server_id, library_id);
