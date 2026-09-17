#!/usr/bin/env bash
#
# Build an AppImage of abs-app for x86_64 or aarch64.
#
# Works both locally (any glibc Linux with the GTK4/libadwaita/GStreamer
# packages installed) and inside CI containers (.github/workflows/release.yml,
# debian:bookworm). The resulting AppImage bundles GTK4, libadwaita, the
# GStreamer plugins and the Adwaita icon theme, so it runs on systems that
# have none of those installed.
#
# Usage:
#   scripts/build-appimage.sh [--arch x86_64|aarch64] [--version V] [--output-dir DIR]
#
# Environment:
#   ABS_APP_BIN         prebuilt release binary to package (skips `cargo build`)
#   LINUXDEPLOY         linuxdeploy executable to use (skips the download)
#   APPIMAGE_TOOLS_DIR  cache dir for the linuxdeploy download
#                       (default: build/appimage-tools)
#
# Bundling strategy:
#   - linuxdeploy resolves and copies every DT_NEEDED library (GTK4, adw,
#     glib, sqlite, openssl, ...) into AppDir/usr/lib and patches rpaths.
#   - Resources that are dlopen()ed and therefore invisible to ldd are copied
#     by this script and handed to linuxdeploy via --deploy-deps-only so their
#     own dependencies get pulled in too: GStreamer plugins, gio modules
#     (TLS, dconf), gdk-pixbuf loaders, GTK4 modules.
#   - The generated apprun-hooks/abs-app.sh is sourced by linuxdeploy's
#     default AppRun and points the runtime at all of the above.

set -euo pipefail

APP_NAME="abs-app"
APP_ID="io.github.gdr-aislop.abs-app"
LINUXDEPLOY_VERSION="1-alpha-20251107-1" # pinned for reproducible builds

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

die() { echo "error: $*" >&2; exit 1; }
warn() { echo "warning: $*" >&2; }

usage() {
    cat <<'USAGE'
Usage: scripts/build-appimage.sh [--arch x86_64|aarch64] [--version V] [--output-dir DIR]

Builds an AppImage of abs-app, bundling GTK4/libadwaita, GStreamer plugins,
glib schemas, pixbuf loaders and the Adwaita icon theme.

Environment:
  ABS_APP_BIN         prebuilt release binary to package (skips `cargo build`)
  LINUXDEPLOY         linuxdeploy executable to use (skips the download)
  APPIMAGE_TOOLS_DIR  cache dir for the linuxdeploy download (default: build/appimage-tools)
USAGE
    exit 0
}

ARCH=""
VERSION=""
OUTPUT_DIR="$REPO_ROOT/build/appimage"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --arch) ARCH="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --output-dir) OUTPUT_DIR="${2:-}"; shift 2 ;;
        --help|-h) usage ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

# --- normalize architecture (linuxdeploy asset naming: x86_64/aarch64) ---
if [ -z "$ARCH" ]; then
    case "$(uname -m)" in
        x86_64|amd64) ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *) die "unsupported architecture: $(uname -m) (use --arch)" ;;
    esac
fi
case "$ARCH" in
    x86_64) LINUXDEPLOY_ASSET="linuxdeploy-x86_64.AppImage" ;;
    aarch64) LINUXDEPLOY_ASSET="linuxdeploy-aarch64.AppImage" ;;
    *) die "unsupported --arch: $ARCH (use x86_64 or aarch64)" ;;
esac

# --- determine version: flag > git describe > Cargo.toml workspace version ---
if [ -z "$VERSION" ]; then
    VERSION="$(git describe --tags --always --dirty 2>/dev/null || true)"
fi
if [ -z "$VERSION" ]; then
    VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
fi
[ -n "$VERSION" ] || die "cannot determine version (pass --version)"
VERSION="${VERSION//\//-}" # keep it filename-safe (branch names can contain /)

# --- tool and source availability checks ---
for tool in cargo pkg-config glib-compile-schemas curl; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found: $tool"
done

# gdk-pixbuf-query-loaders is not on $PATH on Debian/Ubuntu (it ships in
# libgdk-pixbuf-2.0-0 under the multiarch libdir); resolve it explicitly.
if command -v gdk-pixbuf-query-loaders >/dev/null 2>&1; then
    PIXBUF_QUERY="$(command -v gdk-pixbuf-query-loaders)"
else
    PIXBUF_QUERY="$(pkg-config --variable=libdir gdk-pixbuf-2.0 2>/dev/null || true)/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders"
fi
[ -x "$PIXBUF_QUERY" ] || die "gdk-pixbuf-query-loaders not found (is libgdk-pixbuf-2.0-0 installed?)"

# Resolve system paths via pkg-config (works on Debian/Ubuntu layouts and others).
pkg_var() { # <variable> <package> <fallback>
    local value
    value="$(pkg-config --variable="$1" "$2" 2>/dev/null || true)"
    echo "${value:-$3}"
}

SCHEMAS_SRC="$(pkg_var schemasdir gio-2.0 /usr/share/glib-2.0/schemas)"
GIO_MODULES_SRC="$(pkg_var giomoduledir gio-2.0 '')"
PIXBUF_MODULES_SRC="$(pkg_var gdk_pixbuf_moduledir gdk-pixbuf-2.0 '')"
GTK4_MODULES_SRC="$(pkg_var libdir gtk4 '')/gtk-4.0"
# Debian names the variable "pluginsdir" (with scanner dir "pluginscannerdir");
# other distros use "plugindir".
GST_PLUGINS_SRC="$(pkg_var pluginsdir gstreamer-1.0 \
    "$(pkg_var plugindir gstreamer-1.0 "$(pkg_var libdir gstreamer-1.0 '')/gstreamer-1.0")")"

[ -d "$SCHEMAS_SRC" ] || die "glib schemas dir not found ($SCHEMAS_SRC); is libglib2.0-dev installed?"
[ -d "$GIO_MODULES_SRC" ] || die "gio modules dir not found; check gio-2.0.pc / libglib2.0-dev"
[ -d "$PIXBUF_MODULES_SRC" ] || die "gdk-pixbuf loaders dir not found; check gdk-pixbuf-2.0.pc / libgdk-pixbuf-2.0-dev"
[ -d "$GST_PLUGINS_SRC" ] || die "gstreamer plugins dir not found; check gstreamer-1.0.pc / libgstreamer1.0-dev"
ls "$GST_PLUGINS_SRC"/*.so >/dev/null 2>&1 || die "no GStreamer plugins found in $GST_PLUGINS_SRC (install gstreamer1.0-plugins-base/good, gstreamer1.0-libav)"

# GStreamer helper binary (scans plugin metadata); optional but recommended.
GST_SCANNER_SRC=""
for candidate in \
    "$(pkg_var pluginscannerdir gstreamer-1.0 '')/gst-plugin-scanner" \
    "$(pkg_var libexecdir gstreamer-1.0 '')/gstreamer-1.0/gst-plugin-scanner" \
    "$GST_PLUGINS_SRC/../gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" \
    /usr/lib/*/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner; do
    if [ -x "$candidate" ]; then GST_SCANNER_SRC="$candidate"; break; fi
done

# --- build the app (unless a prebuilt binary was supplied) ---
BIN="${ABS_APP_BIN:-${CARGO_TARGET_DIR:-target}/release/abs-app}"
if [ -z "${ABS_APP_BIN:-}" ]; then
    cargo build --release -p abs-app
fi
[ -x "$BIN" ] || die "binary not found at $BIN (set ABS_APP_BIN to override)"

# --- fetch linuxdeploy (x86_64 and aarch64 assets are published upstream) ---
TOOLS_DIR="${APPIMAGE_TOOLS_DIR:-$REPO_ROOT/build/appimage-tools}"
LINUXDEPLOY_BIN="${LINUXDEPLOY:-$TOOLS_DIR/$LINUXDEPLOY_ASSET}"
if [ ! -x "$LINUXDEPLOY_BIN" ]; then
    mkdir -p "$TOOLS_DIR"
    curl -fsSL --retry 3 -o "$LINUXDEPLOY_BIN" \
        "https://github.com/linuxdeploy/linuxdeploy/releases/download/$LINUXDEPLOY_VERSION/$LINUXDEPLOY_ASSET"
    chmod +x "$LINUXDEPLOY_BIN"
fi

# --- assemble the AppDir ---
APPDIR="$OUTPUT_DIR/$APP_NAME.AppDir"
rm -rf "$APPDIR"
mkdir -p "$OUTPUT_DIR"
# Clear previous outputs so the post-build rename can't pick up stale files.
rm -f "$OUTPUT_DIR"/*.AppImage

install -Dm755 "$BIN" "$APPDIR/usr/bin/$APP_NAME"
if command -v strip >/dev/null 2>&1; then
    strip "$APPDIR/usr/bin/$APP_NAME"
fi
install -Dm644 "app/assets/$APP_NAME.desktop" "$APPDIR/usr/share/applications/$APP_NAME.desktop"
install -Dm644 "app/assets/$APP_NAME.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_NAME.svg"
install -Dm644 "app/assets/$APP_ID.metainfo.xml" "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 LICENSE "$APPDIR/usr/share/doc/$APP_NAME/LICENSE"

# GStreamer: core plugins ship in libgstreamer, so copying the whole plugin
# directory covers whatever plugin packages are installed (base/good/libav/...).
mkdir -p "$APPDIR/usr/lib/gstreamer-1.0"
cp -a "$GST_PLUGINS_SRC"/*.so "$APPDIR/usr/lib/gstreamer-1.0/"
if [ -n "$GST_SCANNER_SRC" ]; then
    install -Dm755 "$GST_SCANNER_SRC" "$APPDIR/usr/libexec/gstreamer-1.0/gst-plugin-scanner"
else
    warn "gst-plugin-scanner not found; registry scanning will run in-process"
fi

# gio modules: TLS (glib-networking) and the dconf GSettings backend.
mkdir -p "$APPDIR/usr/lib/gio/modules"
for module in libgiognutls.so libgiolibproxy.so libgiognomeproxy.so libdconfsettings.so; do
    if [ -f "$GIO_MODULES_SRC/$module" ]; then
        cp -a "$GIO_MODULES_SRC/$module" "$APPDIR/usr/lib/gio/modules/"
    else
        warn "gio module not found: $module"
    fi
done

# gdk-pixbuf loaders (png/jpeg/svg covers): copy the loaders and generate the
# cache with bare file names, then symlink the loaders into usr/lib so the
# names resolve through LD_LIBRARY_PATH (same trick as linuxdeploy-plugin-gtk;
# non-relocatable gdk-pixbuf builds resolve relative cache paths via dlopen).
PIXBUF_CACHE="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
PIXBUF_MODULES_DST="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders"
mkdir -p "$PIXBUF_MODULES_DST"
cp -a "$PIXBUF_MODULES_SRC"/*.so "$PIXBUF_MODULES_DST/"
(cd "$PIXBUF_MODULES_DST" && "$PIXBUF_QUERY" ./*.so) \
    | sed -e "s|$PIXBUF_MODULES_DST/||g" -e 's|\./||g' > "$PIXBUF_CACHE"
for loader in "$PIXBUF_MODULES_DST"/*.so; do
    ln -sf "gdk-pixbuf-2.0/2.10.0/loaders/$(basename "$loader")" "$APPDIR/usr/lib/"
done

# GTK4 modules (immodules, print backends); found via GTK_PATH at runtime.
if [ -d "$GTK4_MODULES_SRC" ]; then
    cp -a "$GTK4_MODULES_SRC" "$APPDIR/usr/lib/gtk-4.0"
else
    warn "GTK4 module dir not found at $GTK4_MODULES_SRC; skipping"
fi

# glib settings schemas, compiled against the AppDir copy.
mkdir -p "$APPDIR/usr/share/glib-2.0"
cp -a "$SCHEMAS_SRC" "$APPDIR/usr/share/glib-2.0/schemas"
glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas"

# Icon theme: the app uses -symbolic icons from Adwaita; ship the theme and
# the hicolor index so fallback lookups work without /usr/share on the host.
if [ -d /usr/share/icons/Adwaita ]; then
    cp -a /usr/share/icons/Adwaita "$APPDIR/usr/share/icons/Adwaita"
else
    warn "Adwaita icon theme not found; UI icons will depend on the host"
fi
if [ -f /usr/share/icons/hicolor/index.theme ]; then
    cp -a /usr/share/icons/hicolor/index.theme "$APPDIR/usr/share/icons/hicolor/index.theme"
fi

# Runtime environment: sourced by linuxdeploy's default AppRun.
mkdir -p "$APPDIR/apprun-hooks"
cat > "$APPDIR/apprun-hooks/$APP_NAME.sh" <<'EOF'
#!/usr/bin/env bash
APPDIR="${APPDIR:-"$(readlink -f "$(dirname "${BASH_SOURCE[0]}")/../..")"}"
export APPDIR

export LD_LIBRARY_PATH="$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export GTK_PATH="$APPDIR/usr/lib/gtk-4.0"
export GTK_EXE_PREFIX="$APPDIR/usr" # keep module lookups (immodules, media) inside the bundle
export GSETTINGS_SCHEMA_DIR="$APPDIR/usr/share/glib-2.0/schemas"
export GDK_PIXBUF_MODULE_FILE="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
export GDK_PIXBUF_MODULEDIR="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders"
export GIO_MODULE_DIR="$APPDIR/usr/lib/gio/modules"
export GST_PLUGIN_SYSTEM_PATH="$APPDIR/usr/lib/gstreamer-1.0"
export GST_REGISTRY_FORK=no
if [ -x "$APPDIR/usr/libexec/gstreamer-1.0/gst-plugin-scanner" ]; then
    export GST_PLUGIN_SCANNER="$APPDIR/usr/libexec/gstreamer-1.0/gst-plugin-scanner"
fi
export XDG_DATA_DIRS="$APPDIR/usr/share:/usr/share${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
EOF
chmod +x "$APPDIR/apprun-hooks/$APP_NAME.sh"

# --- let linuxdeploy resolve and bundle shared libraries, then package ---
export APPIMAGE_EXTRACT_AND_RUN=1 # run AppImage tooling without FUSE (containers)
export VERSION="$VERSION"         # read by the appimage output plugin

DEPLOY_ARGS=()
for d in "$APPDIR/usr/lib/gstreamer-1.0" "$APPDIR/usr/lib/gio/modules" \
    "$APPDIR/usr/lib/gdk-pixbuf-2.0" "$APPDIR/usr/lib/gtk-4.0"; do
    if [ -d "$d" ]; then
        DEPLOY_ARGS+=(--deploy-deps-only "$d")
    fi
done

OUTPUT_PATH="$OUTPUT_DIR/$APP_NAME-$VERSION-$ARCH.AppImage"
(
    cd "$OUTPUT_DIR"
    "$LINUXDEPLOY_BIN" \
        --appdir="$APPDIR" \
        --executable="$APPDIR/usr/bin/$APP_NAME" \
        --desktop-file="$APPDIR/usr/share/applications/$APP_NAME.desktop" \
        --icon-file="$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_NAME.svg" \
        "${DEPLOY_ARGS[@]}" \
        --output=appimage
)

# The appimage plugin derives the output file name from the desktop file's
# Name (e.g. "Audiobookshelf-<ver>-<arch>.AppImage"); normalize to ours. The
# output dir was cleared beforehand, so exactly one file can be present.
produced=""
for f in "$OUTPUT_DIR"/*.AppImage; do
    [ -f "$f" ] || continue
    if [ -n "$produced" ]; then
        die "multiple AppImages produced in $OUTPUT_DIR"
    fi
    produced="$f"
done
[ -n "$produced" ] || die "no AppImage produced in $OUTPUT_DIR"
if [ "$produced" != "$OUTPUT_PATH" ]; then
    mv "$produced" "$OUTPUT_PATH"
fi

echo
echo "Built $OUTPUT_PATH ($(du -h "$OUTPUT_PATH" | cut -f1))"
sha256sum "$OUTPUT_PATH"
