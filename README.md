# mobile-linux-audiobookshelf-client

GNOME-native client for an Audiobookshelf media server, designed for mobile Linux
(Phosh on the Librem 5, also usable on desktop). Built with Rust, GTK4 and libadwaita.

## Packaging

Three artifact types are built for every `v*` tag by
[.github/workflows/release.yml](.github/workflows/release.yml), for both
`x86_64` and `aarch64`:

- **`.deb`** — targets PureOS Crimson (Librem 5) and Ubuntu 24.04; expects the
  distro's GTK4/libadwaita/GStreamer packages.
- **`.AppImage`** — bundles GTK4, libadwaita, GStreamer plugins and the Adwaita
  icon theme; runs on any glibc distro without installing dependencies.
- **Flatpak** — built against `org.gnome.Platform`, published both as a
  downloadable `.flatpak` bundle attached to the release and as a live,
  signed remote hosted on GitHub Pages (see below).
- **`.deb`** is, likewise, also published as a live, signed APT repository
  on the same GitHub Pages site, alongside the downloadable file attached
  to the release.

The `.deb` and `.AppImage` are built inside a Debian bookworm container so
they link against the library baseline of the target distros.

### Installing the Flatpak

The recommended way — adds a remote once, then `flatpak update` picks up
every new tagged release automatically:

```sh
flatpak remote-add --user --if-not-exists abs-app \
  https://gdr-aislop.github.io/mobile-linux-audiobookshelf-client/io.github.gdr-aislop.abs-app.flatpakrepo
flatpak install --user abs-app io.github.gdr-aislop.abs-app
```

Alternatively, download the `.flatpak` file from a
[release](../../releases) and install it directly, with no remote added:

```sh
flatpak install --user ./abs-app-<version>-<arch>.flatpak
```

### Installing the .deb via APT

The recommended way — adds the repo once, then `apt upgrade` picks up every
new tagged release automatically (arm64 and amd64 only):

```sh
sudo curl -fsSL https://gdr-aislop.github.io/mobile-linux-audiobookshelf-client/apt/abs-app.asc \
  -o /etc/apt/keyrings/abs-app.asc
echo "deb [signed-by=/etc/apt/keyrings/abs-app.asc] https://gdr-aislop.github.io/mobile-linux-audiobookshelf-client/apt stable main" \
  | sudo tee /etc/apt/sources.list.d/abs-app.list
sudo apt update
sudo apt install abs-app
```

This repo always carries the *latest* tagged release only (not a full
version history) — older `.deb`s stay available from
[past releases](../../releases) directly, via `sudo apt install
./abs-app_<version>_<arch>.deb`.

### Building an AppImage manually

```sh
# Inside a Debian-family system with the dev + runtime packages installed:
./scripts/build-appimage.sh                       # x86_64, version from git
./scripts/build-appimage.sh --arch aarch64 --version 0.1.0

# Anywhere, via a Debian bookworm container (docker or podman):
./scripts/build-appimage-in-docker.sh

# aarch64 AppImage on an x86_64 host (arm64 container under QEMU emulation):
./scripts/build-aarch64-appimage-on-x86.sh
```

Output lands in `build/appimage/abs-app-<version>-<arch>.AppImage`.
`scripts/build-appimage.sh --help` documents environment overrides
(`ABS_APP_BIN`, `LINUXDEPLOY`, `APPIMAGE_TOOLS_DIR`); the docker wrapper
mirrors the CI job's package set via `scripts/appimage-builder-bookworm.Dockerfile`.

## Diagnosing a crash report

The app never phones home; everything below stays on the user's device. If
someone reports a crash, ask for whatever exists under
`~/.local/state/io.github.gdr_aislop.Audiobookshelf/`:

- `logs/abs-app.*` — a rotating, human-readable log file (last 7 days kept).
  A Rust panic always logs its full backtrace here, even without
  `RUST_BACKTRACE` set.
- `crashes/crash-<timestamp>-<pid>.dmp` — a Breakpad-format minidump written
  for a native (signal-level) crash a Rust panic hook can't catch (a
  segfault/abort inside GTK/GStreamer/glib). It's binary, not directly
  readable: turn it into a stack trace with
  [`minidump-stackwalk`](https://crates.io/crates/minidump-stackwalk)
  (`cargo install minidump-stackwalk`), which also needs debug symbols from
  a build matching the exact version that crashed (an unstripped binary, or
  a separately retained `.debug` file).
