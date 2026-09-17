#!/usr/bin/env bash
#
# Run scripts/build-appimage.sh inside a Debian bookworm container, so local
# builds use the same library baseline as the CI builds (and the target
# distros). Arguments are passed through to the build script:
#
#   scripts/build-appimage-in-docker.sh [--arch x86_64|aarch64] [--version V] ...
#
# The first invocation pulls debian:bookworm and installs the dependencies
# into a locally cached image; later runs only re-run changed layers. The
# cargo download cache lives in ~/.cache/abs-appimage-build/cargo, and builds
# use a target dir separate from host builds (build/appimage-target).
#
# This wrapper is a developer convenience, not used by CI:
# .github/workflows/release.yml runs the build script directly inside its own
# debian:bookworm container job.

set -euo pipefail

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

die() { echo "error: $*" >&2; exit 1; }

ENGINE=""
for candidate in docker podman; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" info >/dev/null 2>&1; then
        ENGINE="$candidate"
        break
    fi
done
[ -n "$ENGINE" ] || die "no working docker or podman found (is the daemon running?)"

IMAGE="abs-appimage-builder:bookworm"
$ENGINE build -t "$IMAGE" -f scripts/appimage-builder-bookworm.Dockerfile .

CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/abs-appimage-build"
mkdir -p "$CACHE_DIR/cargo"

REPO_MOUNT="$REPO_ROOT:/repo"
# SELinux hosts (Fedora/RHEL with podman) need relabeling for the bind mount.
if [ "$ENGINE" = "podman" ] && command -v getenforce >/dev/null 2>&1 \
    && [ "$(getenforce 2>/dev/null || true)" = "Enforcing" ]; then
    REPO_MOUNT="$REPO_ROOT:/repo:Z"
fi

exec "$ENGINE" run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo-home \
    -e CARGO_TARGET_DIR=/repo/build/appimage-target \
    -w /repo \
    -v "$REPO_MOUNT" \
    -v "$CACHE_DIR/cargo:/cargo-home" \
    "$IMAGE" \
    ./scripts/build-appimage.sh "$@"
