-- Whether a progress row holds a local write the server hasn't confirmed yet. Set by every local
-- write, cleared once a push of that same value succeeds, and 0 for rows imported from the
-- server. Without it, a position recorded offline was only ever pushed while that same book stayed
-- loaded in the player; after a restart it sat locally until another device's older position,
-- being "newer" on the server, overwrote it.
ALTER TABLE progress ADD COLUMN needs_push INTEGER NOT NULL DEFAULT 0 CHECK (needs_push IN (0, 1));
