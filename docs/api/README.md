# Talking to Audiobookshelf from Rust

The server's REST API is fully described by an OpenAPI 3.0 spec, vendored at
[`vendor/audiobookshelf-openapi/openapi.json`](../../vendor/audiobookshelf-openapi/openapi.json).
It's clean: 46 operations across 31 paths, every operation has an `operationId`, and auth is a
single bearer token — good conditions for generating a typed client instead of hand-writing one.

## Recommended approach: generate the client with `progenitor`

[`progenitor`](https://github.com/oxidecomputer/progenitor) (Oxide Computer) turns an OpenAPI spec
into a fully-typed, async Rust client built on `reqwest`: one method per operation, request/response
structs for every schema, and a builder-style call site (`client.get_library_items(id).send().await?`).

This was verified against the vendored spec in this repo: `progenitor::Generator::default()`
generates without error, and the generated code compiles cleanly against `progenitor-client 0.9`,
`reqwest 0.12`, `serde`/`serde_json`, `uuid`, `chrono`, and `schemars 0.8`.

### Setup

Add a dedicated crate (e.g. `abs-api/`) that generates the client at build time via `build.rs`,
rather than committing ~400 KB of generated code:

```toml
# abs-api/Cargo.toml
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
// abs-api/build.rs
use std::{env, fs, path::Path};

fn main() {
    let spec_path = "../vendor/audiobookshelf-openapi/openapi.json";
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
// abs-api/src/lib.rs
include!(concat!(env!("OUT_DIR"), "/client.rs"));
```

### Usage

```rust
let client = abs_api::Client::new("https://library.homeserver.dev");
// Bearer token from the login flow (POST /login):
let client = client.with_bearer_token(token);

let libraries = client.get_libraries().send().await?.into_inner();
let items = client
    .get_library_items(&library_id)
    .send()
    .await?
    .into_inner();
```

Generated method names come straight from the spec's `operationId`s (e.g. `get_library_items`,
`update_media_progress`, `send_e_book_to_device`), so the vendored spec doubles as the reference
for what's callable.

## Why not a hand-written client?

A hand-rolled `reqwest` + `serde` client is the fallback if a future spec version turns out messy
(missing `operationId`s, schema conflicts, etc.) — but that isn't the case today, and generating
means the client tracks the server's actual contract instead of a hand-maintained approximation
that silently drifts. If `progenitor` output ever needs a manual override for one endpoint, that's
a small, local exception, not a reason to abandon codegen for the other 45 operations.

## Updating the client

Bump `vendor/audiobookshelf-openapi/openapi.json` (see its own README for the update steps) and
rebuild — `build.rs` regenerates automatically since it's keyed off that file's contents. Check the
new spec still upholds the two properties above (`operationId` on every operation, no naming
conflicts) before trusting a clean `cargo build`.
