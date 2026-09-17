-- Accounts gain the OAuth-style refresh token from Audiobookshelf's JWT auth system (server
-- v2.26.0+): login with `x-return-tokens: true` returns an access token (short-lived) plus a
-- refresh token (long-lived, rotated on every refresh), and only the pair together keeps a
-- session alive past the access token's expiry. NULL for legacy servers (permanent tokens,
-- no expiry) — the auth layer treats a missing refresh token as "nothing to refresh".
ALTER TABLE accounts ADD COLUMN refresh_token TEXT;
