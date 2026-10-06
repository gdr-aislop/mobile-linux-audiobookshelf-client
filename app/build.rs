//! Captures the short git commit hash at compile time as the `ABS_APP_GIT_HASH` env var, read by
//! `application::version_line()` and main's startup diagnostics log line — so a built binary
//! (and a user's pasted log) identifies exactly which commit it came from, without anyone having
//! to ask. Falls back to `"unknown"` when this isn't a git checkout (a distro source tarball
//! build) or `git` isn't on `PATH`, rather than failing the build over a cosmetic detail.
//!
//! No `cargo:rerun-if-changed` directive is emitted on purpose: that would restrict reruns to
//! only the paths named, and getting every case right (`.git/HEAD` for a normal checkout,
//! whichever ref it points at, a detached HEAD, a worktree's separate `.git` file) isn't worth
//! it for what's already a rarely-rebuilt, cheap-to-recompute version stamp — emitting nothing
//! means cargo reruns this script on every build instead, which is exactly what keeps the hash
//! honest.
//!
//! It also compiles `data/resources.gresource.xml` (the icons the system theme can't be relied on
//! for, see `crate::icons`) into `$OUT_DIR/abs-app.gresource` with `glib-compile-resources`. Unlike
//! the hash, that one is not optional: without it the build fails, rather than shipping a binary
//! that shows "missing icon" placeholders. The tool comes with GLib's development files
//! (`libglib2.0-dev-bin`, pulled in by `libgtk-4-dev`; the GNOME SDK has it too).

use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=ABS_APP_GIT_HASH={hash}");

    compile_resources();
}

fn compile_resources() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir", "data", "--target"])
        .arg(std::path::Path::new(&out_dir).join("abs-app.gresource"))
        .arg("data/resources.gresource.xml")
        .status()
        .unwrap_or_else(|err| panic!("could not run glib-compile-resources (install libglib2.0-dev-bin): {err}"));
    assert!(status.success(), "glib-compile-resources failed on data/resources.gresource.xml ({status})");
}
