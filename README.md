# mobile-linux-audiobookshelf-client

GNOME-native client for an Audiobookshelf media server, designed for mobile Linux
(Phosh on the Librem 5, also usable on desktop). Built with Rust, GTK4 and libadwaita.

## Packaging

Two artifact types are built for every `v*` tag by
[.github/workflows/release.yml](.github/workflows/release.yml), for both
`x86_64` and `aarch64`:

- **`.deb`** — targets PureOS Crimson (Librem 5) and Ubuntu 24.04; expects the
  distro's GTK4/libadwaita/GStreamer packages.
- **`.AppImage`** — bundles GTK4, libadwaita, GStreamer plugins and the Adwaita
  icon theme; runs on any glibc distro without installing dependencies.

Both are built inside a Debian bookworm container so they link against the
library baseline of the target distros.

### Building an AppImage manually

```sh
# Inside a Debian-family system with the dev + runtime packages installed:
./scripts/build-appimage.sh                       # x86_64, version from git
./scripts/build-appimage.sh --arch aarch64 --version 0.1.0

# Anywhere, via a Debian bookworm container (docker or podman):
./scripts/build-appimage-in-docker.sh
```

Output lands in `build/appimage/abs-app-<version>-<arch>.AppImage`.
`scripts/build-appimage.sh --help` documents environment overrides
(`ABS_APP_BIN`, `LINUXDEPLOY`, `APPIMAGE_TOOLS_DIR`); the docker wrapper
mirrors the CI job's package set via `scripts/appimage-builder-bookworm.Dockerfile`.
