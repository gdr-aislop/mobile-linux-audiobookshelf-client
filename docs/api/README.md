# Talking to Audiobookshelf from Rust

Part of the server's REST API is described by an OpenAPI 3.0 spec, vendored at
[`third_party/audiobookshelf-openapi/openapi.json`](../../third_party/audiobookshelf-openapi/openapi.json).
Where it has coverage it's clean — 46 operations across 31 paths, every operation has an
`operationId`, auth is a single bearer token — but **coverage is partial**: libraries, authors,
series, podcasts, notifications, and email/e-reader settings are documented; item detail
(`/api/items/*`), the current user/progress/bookmarks (`/api/me/*`), and authentication are not.
See `third_party/audiobookshelf-openapi/README.md`'s "Known gaps" section — confirmed by diffing
the spec against the real route table in the server source, not assumed. "Handling endpoints the
spec doesn't cover" below covers what to do about it.

## Recommended approach: generate the client with `progenitor`

[`progenitor`](https://github.com/oxidecomputer/progenitor) (Oxide Computer) turns an OpenAPI spec
into a fully-typed, async Rust client built on `reqwest`: one method per operation (named from its
`operationId`), and request/response structs for every schema. With
`progenitor::Generator::default()` (the config this crate uses), each operation becomes a plain
`async fn` returning `Result<ResponseValue<T>, Error<E>>` directly — there is no separate
builder/`.send()` step.

This was verified against the vendored spec in this repo, not just assumed: `abs-api` actually
builds and its test suite (`cargo test -p abs-api`) passes, including real HTTP round-trips against
a `wiremock` server exercising `get_libraries` and `get_library_by_id` — proving the generated
client sends requests to the right paths and correctly deserializes real response bodies, not just
that it type-checks.

### Setup

Add a dedicated crate (`crates/abs-api/`) that generates the client at build time via `build.rs`,
rather than committing ~400 KB of generated code:

```toml
# crates/abs-api/Cargo.toml
[package]
name = "abs-api"
version = "0.1.0"
edition = "2021"

[build-dependencies]
progenitor = "0.9"
openapiv3 = "2"
serde_json = "1"
syn = "2"
prettyplease = "0.2"

[dependencies]
progenitor-client = "0.9"
reqwest = { version = "0.12", features = ["json", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde", "v4"] }
chrono = { version = "0.4", features = ["serde"] }
schemars = "0.8"
```

```rust
// crates/abs-api/build.rs
use std::{env, fs, path::Path};

fn main() {
    let spec_path = "../../third_party/audiobookshelf-openapi/openapi.json";
    println!("cargo:rerun-if-changed={spec_path}");

    let src = fs::read_to_string(spec_path).expect("read vendored openapi spec");
    let spec: openapiv3::OpenAPI = serde_json::from_str(&src).expect("parse openapi spec");

    let mut generator = progenitor::Generator::default();
    let tokens = generator.generate_tokens(&spec).expect("generate client");
    let ast: syn::File = syn::parse2(tokens).expect("parse generated tokens");
    let content = prettyplease::unparse(&ast);

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("client.rs"), content).unwrap();
}
```

```rust
// crates/abs-api/src/lib.rs
include!(concat!(env!("OUT_DIR"), "/client.rs"));
```

### Usage

There is no `with_bearer_token`-style builder on `Client` — the generated methods read auth from
whatever `reqwest::Client` the `abs_api::Client` wraps, so the bearer token is set once via
`reqwest`'s default headers, not per call:

```rust
let mut headers = reqwest::header::HeaderMap::new();
headers.insert(
    reqwest::header::AUTHORIZATION,
    reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
);
let http = reqwest::Client::builder().default_headers(headers).build()?;
let client = abs_api::Client::new_with_client("https://library.homeserver.dev", http);

// Every operation is a plain async fn — no `.send()` step — returning `ResponseValue<T>`,
// which `.into_inner()` unwraps to the typed response body:
let libraries = client.get_libraries().await?.into_inner();

// Optional query params are positional `Option<_>` arguments, in the order the spec declares
// them (collapse_series, desc, filter, include, limit, minified, page, sort for this one):
let items = client
    .get_library_items(&library_id, None, None, None, None, None, None, None, None)
    .await?
    .into_inner();
```

Generated method names come straight from the spec's `operationId`s (e.g. `get_library_items`,
`get_library_authors`, `send_e_book_to_device`), so the vendored spec doubles as the reference for
what's callable — for anything under `/api/items/*` or `/api/me/*`, it isn't in this client (see
"Handling endpoints the spec doesn't cover" below).

## Why not a hand-written client?

A hand-rolled `reqwest` + `serde` client is the fallback if a future spec version turns out messy
(missing `operationId`s, schema conflicts, etc.) — but that isn't the case today, and generating
means the client tracks the server's actual contract instead of a hand-maintained approximation
that silently drifts. If `progenitor` output ever needs a manual override for one endpoint, that's
a small, local exception, not a reason to abandon codegen for the other 45 operations.

## Handling endpoints the spec doesn't cover

`abs-core` needs item detail, playback progress, and login — none of which `abs-api` exposes,
since they're missing from the vendored spec entirely (see the gaps note above). Rather than
hand-maintaining a parallel spec fragment for them, `abs-core` calls these directly with a plain
`reqwest::Client` (the same one `abs-api`'s `Client` wraps, so auth headers and base URL stay
shared) against the real routes confirmed in the server source
(`server/routers/ApiRouter.js`): `GET /api/items/:id`, `POST /api/items/:id/play`,
`PATCH /api/items/:id/media`, `GET /api/me/progress`, and whatever `/login` turns out to be. These
calls are untyped at the OpenAPI-codegen level — model their request/response shapes as plain
`serde` structs in `abs-core` itself, next to the logic that uses them, rather than inventing a
second generated-client layer for a handful of endpoints. If upstream ever documents them, they
move into `abs-api` and the hand-written versions in `abs-core` are deleted.

## Updating the client

Bump `third_party/audiobookshelf-openapi/openapi.json` (see its own README for the update steps) and
rebuild — `build.rs` regenerates automatically since it's keyed off that file's contents. Check the
new spec still upholds the two properties above (`operationId` on every operation, no naming
conflicts) before trusting a clean `cargo build`.
