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

## Updating

1. Pull the latest `docs/openapi.json` from the upstream repo (pin to a specific commit/tag, not
   just `main`, so updates are deliberate).
2. Replace this file, update the commit hash/server version above.
3. Regenerate the `abs-api` crate (see `docs/api/README.md`) and fix any compile errors the diff
   introduces — Audiobookshelf's spec has been clean so far (no missing `operationId`s, no schema
   conflicts), but that isn't guaranteed to hold across versions.
