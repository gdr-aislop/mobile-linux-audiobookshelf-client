#!/usr/bin/env bash
#
# Generate flatpak/cargo-sources.json from Cargo.lock, so the Flatpak build
# (which runs in a network-less sandbox) can `cargo --offline build` against
# a vendored copy of every crate instead of fetching from crates.io.
#
# Regenerate this whenever Cargo.lock changes; it is not committed (kept out
# of version control the same way target/ and build/ are) and CI always
# regenerates it fresh before invoking flatpak-builder, so it can never go
# stale relative to Cargo.lock.
#
# Usage: scripts/gen-flatpak-cargo-sources.sh
# Writes: flatpak/cargo-sources.json

set -euo pipefail

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

die() { echo "error: $*" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || die "python3 not found"

GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"
GENERATOR="$REPO_ROOT/build/flatpak-cargo-generator.py"
mkdir -p "$(dirname "$GENERATOR")"
if [ ! -f "$GENERATOR" ]; then
    curl -fsSL --retry 3 -o "$GENERATOR" "$GENERATOR_URL"
fi

# The generator declares its own dependencies via PEP 723 inline script
# metadata; prefer `uv run` (installs them into an ephemeral env) and fall
# back to a plain venv + pip install for environments without uv.
mkdir -p flatpak
if command -v uv >/dev/null 2>&1; then
    uv run --no-project "$GENERATOR" Cargo.lock -o flatpak/cargo-sources.json
else
    VENV="$REPO_ROOT/build/flatpak-cargo-generator-venv"
    if [ ! -d "$VENV" ]; then
        python3 -m venv "$VENV"
        "$VENV/bin/pip" install --quiet "aiohttp<4.0.0,>=3.9.5" "PyYAML<7.0.0,>=6.0.2" "tomlkit>=0.13.3,<1.0"
    fi
    "$VENV/bin/python3" "$GENERATOR" Cargo.lock -o flatpak/cargo-sources.json
fi

echo "Wrote flatpak/cargo-sources.json"
