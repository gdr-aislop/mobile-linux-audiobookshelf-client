# Audiobookshelf OpenAPI spec

`openapi.json` is vendored, unmodified, from the
[advplyr/audiobookshelf](https://github.com/advplyr/audiobookshelf) server repository at
`docs/openapi.json`.

- **Source commit**: `a06639886b7c5b52e5d906ff58e1c2471aa12981` (2026-09-14)
- **Server version**: 2.36.0 (from that commit's `package.json`)
- **OpenAPI version**: 3.0.0 — 46 operations across 31 paths, all with `operationId`s, bearer-token
  auth (`BearerAuth`)
- **License**: GPL-3.0, same as this project and upstream Audiobookshelf

## Why vendor it

The client's Rust API layer is generated from this file (see the workspace's `abs-api` crate, or
`docs/api/README.md` for the plan if that crate doesn't exist yet). Vendoring pins us to a spec
we've verified generates and compiles cleanly, independent of whether `api.audiobookshelf.org` or
the upstream repo path changes.

## Known gaps

The spec is clean where it has coverage, but that coverage is partial. Confirmed by diffing this
file's paths against the real route table in the server source
(`server/routers/ApiRouter.js` at the same commit): the spec documents libraries, authors, series,
podcasts, notifications, and email/e-reader settings, but has **no entries at all** for:

- **`/api/items/*`** — item detail, media/tracks/chapters updates, cover art, downloads, playback
  session start (`findOne`, `updateMedia`, `download`, `play`, etc.)
- **`/api/me/*`** — the current user, media progress, bookmarks, listening sessions/stats
- **Authentication/login** — no auth endpoints are documented at all (only `BearerAuth` as a
  security *scheme*, with nothing to obtain a token from)

These are exactly the endpoints this client needs for item detail, playback progress, and
downloads, so `abs-core` cannot rely on `abs-api` alone — see `docs/api/README.md`'s note on
handling calls outside the vendored spec's coverage. This is a real product of Audiobookshelf's
own incremental OpenAPI documentation effort, not a vendoring mistake on our part; re-check this
list against upstream's route table whenever the spec is updated, since new coverage may close
some of these gaps over time.

## Updating

1. Pull the latest `docs/openapi.json` from the upstream repo (pin to a specific commit/tag, not
   just `main`, so updates are deliberate).
2. Replace this file, update the commit hash/server version above.
3. Regenerate the `abs-api` crate (see `docs/api/README.md`) and fix any compile errors the diff
   introduces — Audiobookshelf's spec has been clean so far (no missing `operationId`s, no schema
   conflicts), but that isn't guaranteed to hold across versions.
4. Re-check the "Known gaps" list above against `server/routers/ApiRouter.js` at the new commit —
   note any endpoints that gained (or lost) OpenAPI coverage.
