//! Resolves the on-disk locations this app uses, following the XDG Base Directory layout
//! documented in the architecture plan: durable user data under `$XDG_DATA_HOME`, evictable
//! cache under `$XDG_CACHE_HOME`, and operational state (logs, crash dumps — not user data, not
//! safe to evict like a cache) under `$XDG_STATE_HOME`. Nothing outside this module computes one
//! of these paths directly.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// The app's reverse-DNS identity — also the GTK application id and, on Flatpak, the sandbox's
/// per-app data directory name, so this is deliberately used verbatim as the directory name
/// below rather than through `directories::ProjectDirs`. `ProjectDirs::from(qualifier,
/// organization, application)` looks like the natural fit, but on Linux it ignores `qualifier`
/// and `organization` entirely and just lowercases `application` — confirmed by actually running
/// it, not assumed — which would have put this app's data under `~/.local/share/audiobookshelf/`
/// instead of the reverse-DNS-named directory GNOME apps (and Flatpak) actually use.
///
/// Matches the Flatpak manifest's `app-id` (`flatpak/io.github.gdr-aislop.abs-app.json`) exactly —
/// a hyphen mid-element is valid in a `GApplication` id (`g_application_id_is_valid` only forbids
/// a leading hyphen per element, confirmed against GLib's own docs), so there's no reason for this
/// to diverge from the id every other part of the app's identity (AppStream metainfo, the Flatpak
/// manifest itself) already uses.
pub const APP_ID: &str = "io.github.gdr-aislop.abs-app";

#[derive(Debug, Clone)]
pub struct AppPaths {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    state_dir: PathBuf,
}

impl AppPaths {
    /// Resolve paths from the real XDG environment. Returns `None` if no home directory can be
    /// determined at all (extremely unusual — `directories` falls back sensibly otherwise).
    pub fn resolve() -> Option<Self> {
        let dirs = BaseDirs::new()?;
        Some(Self {
            data_dir: dirs.data_dir().join(APP_ID),
            cache_dir: dirs.cache_dir().join(APP_ID),
            // `state_dir()` is `None` on platforms with no `$XDG_STATE_HOME` equivalent — falls
            // back to `data_dir` (durable-adjacent) rather than `cache_dir` (evictable), matching
            // why logs/crash dumps don't belong in the cache dir in the first place.
            state_dir: dirs.state_dir().unwrap_or_else(|| dirs.data_dir()).join(APP_ID),
        })
    }

    /// Build an instance rooted at arbitrary directories — used by tests (and could be used for
    /// a future multi-profile mode) instead of touching the real user environment.
    pub fn rooted_at(data_dir: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            cache_dir: cache_dir.into(),
            state_dir: state_dir.into(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Rotating application logs — see `crash_reporting::init_logging` in the `app` crate.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    /// Local Breakpad-format `.dmp` files written on a native (signal-level) crash — see
    /// `crash_reporting::attach_crash_handler` in the `app` crate.
    pub fn crash_dumps_dir(&self) -> PathBuf {
        self.state_dir.join("crashes")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("db.sqlite3")
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    /// Directory holding downloaded chapter files for one item on one server.
    pub fn item_downloads_dir(&self, server_id: &str, item_id: &str) -> PathBuf {
        self.downloads_dir().join(server_id).join(item_id)
    }

    /// Path a given track (audio file, keyed by the server's own `ino`) of an item would be
    /// downloaded to. `extension` must be the real extension for the track's content-type (e.g.
    /// `"mp3"`, `"m4b"`, `"m4a"`) — Audiobookshelf items aren't always mp3, so callers must derive
    /// it from the actual response the same way `cover_cache_path` already requires for covers.
    pub fn track_file_path(&self, server_id: &str, item_id: &str, ino: &str, extension: &str) -> PathBuf {
        self.item_downloads_dir(server_id, item_id).join(format!("{ino}.{extension}"))
    }

    pub fn covers_dir(&self) -> PathBuf {
        self.cache_dir.join("covers")
    }

    /// `extension` is the real extension for the cover bytes being cached (e.g. `"webp"`,
    /// `"png"`, `"jpg"`) — Audiobookshelf serves covers in whatever format the source file is in,
    /// not always JPEG, so callers must pass the actual content-type-derived extension rather
    /// than assuming one.
    pub fn cover_cache_path(&self, server_id: &str, item_id: &str, extension: &str) -> PathBuf {
        self.covers_dir()
            .join(server_id)
            .join(format!("{item_id}.{extension}"))
    }

    /// Every directory this `AppPaths` might write into — the single list `ensure_dirs` (async,
    /// called once `setup()` has a Tokio runtime) and `main.rs`'s early synchronous creation of
    /// just the logging/crash-dump dirs (needed before a runtime exists) both draw from, so the
    /// two can't drift apart.
    fn all_dirs(&self) -> [PathBuf; 6] {
        [
            self.data_dir.clone(),
            self.downloads_dir(),
            self.cache_dir.clone(),
            self.covers_dir(),
            self.logs_dir(),
            self.crash_dumps_dir(),
        ]
    }

    /// Create every directory this `AppPaths` might write into. Idempotent.
    pub async fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in self.all_dirs() {
            tokio::fs::create_dir_all(dir).await?;
        }
        Ok(())
    }

    /// Synchronous, `logs_dir`/`crash_dumps_dir`-only subset of `ensure_dirs` — for `main.rs` to
    /// call before a Tokio runtime exists, since logging and crash-dump capture must be wired up
    /// before the rest of async setup runs (so they can catch failures during that setup too).
    /// `ensure_dirs` still creates these same two directories again later (idempotent) alongside
    /// everything else.
    pub fn ensure_early_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.crash_dumps_dir())?;
        Ok(())
    }

    /// Delete everything on disk scoped to one server — its cover-cache subtree
    /// (`covers/<server_id>/`) and its downloads subtree (`downloads/<server_id>/`). Used when a
    /// re-login switches to a different server and the old one's cached files would otherwise sit
    /// orphaned forever. Missing directories are fine (nothing was ever downloaded); any other
    /// error surfaces to the caller, which decides it's non-fatal — stale files are wasteful, not
    /// harmful.
    pub async fn purge_server_data(&self, server_id: &str) -> std::io::Result<()> {
        let mut first_error = None;
        for dir in [self.downloads_dir().join(server_id), self.covers_dir().join(server_id)] {
            if let Err(err) = tokio::fs::remove_dir_all(&dir).await {
                if err.kind() != std::io::ErrorKind::NotFound && first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// Bytes available to this (unprivileged) process on the filesystem holding `path` — what the
/// download sheet's size estimates are compared against (ui-spec: an option whose estimate
/// exceeds free space is refused up front instead of failing partway through). `None` when it
/// can't be determined (the path doesn't exist yet, an exotic filesystem) — callers must treat
/// that as "don't restrict", never as "zero": an unknown free space must not block downloads.
pub fn free_space_bytes(path: &Path) -> Option<u64> {
    let stats = rustix::fs::statvfs(path).ok()?;
    Some(stats.f_bavail.saturating_mul(stats.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"), tmp.path().join("state"));
        (tmp, paths)
    }

    #[test]
    fn free_space_is_some_positive_for_an_existing_dir_and_none_for_a_missing_one() {
        let tmp = tempfile::tempdir().unwrap();
        let free = free_space_bytes(tmp.path());
        assert!(free.is_some_and(|bytes| bytes > 0), "a real directory always has some free space");

        assert_eq!(free_space_bytes(&tmp.path().join("does-not-exist")), None, "a missing path can't be probed — treated as unknown, not zero");
    }

    #[test]
    fn db_path_lives_under_data_dir() {
        let (_tmp, paths) = test_paths();
        assert_eq!(paths.db_path(), paths.data_dir().join("db.sqlite3"));
    }

    #[test]
    fn item_downloads_dir_is_scoped_by_server_and_item() {
        let (_tmp, paths) = test_paths();
        let a = paths.item_downloads_dir("server-a", "item-1");
        let b = paths.item_downloads_dir("server-b", "item-1");
        let c = paths.item_downloads_dir("server-a", "item-2");
        assert_ne!(a, b, "different servers must not share a download directory");
        assert_ne!(a, c, "different items must not share a download directory");
        assert!(a.starts_with(paths.downloads_dir()));
    }

    #[test]
    fn track_file_path_is_named_after_the_ino_with_the_given_extension() {
        let (_tmp, paths) = test_paths();
        let mp3 = paths.track_file_path("s", "i", "12345", "mp3");
        let m4b = paths.track_file_path("s", "i", "67890", "m4b");
        assert_eq!(mp3.file_name().unwrap(), "12345.mp3");
        assert_eq!(m4b.file_name().unwrap(), "67890.m4b");
        assert!(mp3.starts_with(paths.item_downloads_dir("s", "i")));
    }

    #[test]
    fn cover_cache_path_lives_under_cache_dir_not_data_dir() {
        let (_tmp, paths) = test_paths();
        let cover = paths.cover_cache_path("server-a", "item-1", "jpg");
        assert!(cover.starts_with(paths.cache_dir()));
        assert!(!cover.starts_with(paths.data_dir()));
    }

    #[test]
    fn logs_dir_and_crash_dumps_dir_live_under_state_dir_not_cache_or_data() {
        let (_tmp, paths) = test_paths();
        for dir in [paths.logs_dir(), paths.crash_dumps_dir()] {
            assert!(dir.starts_with(paths.state_dir()));
            assert!(!dir.starts_with(paths.cache_dir()));
            assert!(!dir.starts_with(paths.data_dir()));
        }
        assert_ne!(paths.logs_dir(), paths.crash_dumps_dir());
    }

    #[test]
    fn cover_cache_path_uses_the_given_extension() {
        let (_tmp, paths) = test_paths();
        let cover = paths.cover_cache_path("server-a", "item-1", "webp");
        assert_eq!(cover.extension().unwrap(), "webp");
    }

    #[tokio::test]
    async fn ensure_dirs_creates_every_directory_it_promises() {
        let (_tmp, paths) = test_paths();
        paths.ensure_dirs().await.expect("ensure_dirs succeeds");

        assert!(paths.data_dir().is_dir());
        assert!(paths.downloads_dir().is_dir());
        assert!(paths.cache_dir().is_dir());
        assert!(paths.covers_dir().is_dir());
        assert!(paths.logs_dir().is_dir());
        assert!(paths.crash_dumps_dir().is_dir());
    }

    #[test]
    fn ensure_early_dirs_creates_only_logs_and_crash_dumps() {
        let (_tmp, paths) = test_paths();
        paths.ensure_early_dirs().expect("ensure_early_dirs succeeds");

        assert!(paths.logs_dir().is_dir());
        assert!(paths.crash_dumps_dir().is_dir());
        assert!(!paths.data_dir().exists(), "ensure_early_dirs must not create the other, async-created dirs");
    }
    #[tokio::test]
    async fn ensure_dirs_is_idempotent() {
        let (_tmp, paths) = test_paths();
        paths.ensure_dirs().await.expect("first call succeeds");

        paths
            .ensure_dirs()
            .await
            .expect("second call on already-existing dirs also succeeds");
    }

    #[tokio::test]
    async fn purge_server_data_removes_only_that_servers_subtrees() {
        let (_tmp, paths) = test_paths();
        paths.ensure_dirs().await.unwrap();

        let old_cover = paths.cover_cache_path("old-server", "item-1", "jpg");
        tokio::fs::create_dir_all(old_cover.parent().unwrap()).await.unwrap();
        tokio::fs::write(&old_cover, b"bytes").await.unwrap();
        let old_download = paths.item_downloads_dir("old-server", "item-1");
        tokio::fs::create_dir_all(&old_download).await.unwrap();
        tokio::fs::write(old_download.join("001-ch.mp3"), b"bytes").await.unwrap();
        let kept_cover = paths.cover_cache_path("kept-server", "item-1", "jpg");
        tokio::fs::create_dir_all(kept_cover.parent().unwrap()).await.unwrap();
        tokio::fs::write(&kept_cover, b"bytes").await.unwrap();

        paths.purge_server_data("old-server").await.expect("purge succeeds");

        assert!(!old_cover.exists(), "the old server's covers must go");
        assert!(!old_download.exists(), "the old server's downloads must go");
        assert!(kept_cover.exists(), "other servers' files must be untouched");
    }

    #[tokio::test]
    async fn purge_server_data_tolerates_a_server_that_never_downloaded_anything() {
        let (_tmp, paths) = test_paths();
        paths.ensure_dirs().await.unwrap();

        paths.purge_server_data("no-such-server").await.expect("missing dirs are a no-op, not an error");
    }

    #[test]
    fn resolve_produces_distinct_data_cache_and_state_dirs() {
        // This touches the real environment (HOME/XDG_*), so just check internal consistency
        // rather than asserting a specific path — the sandbox running this test may not have a
        // conventional home directory.
        if let Some(paths) = AppPaths::resolve() {
            assert_ne!(paths.data_dir(), paths.cache_dir());
            assert_ne!(paths.data_dir(), paths.state_dir());
            assert_ne!(paths.cache_dir(), paths.state_dir());
        }
    }

    #[test]
    fn resolve_uses_the_literal_app_id_as_the_directory_name() {
        // Regression test: `ProjectDirs::from(qualifier, org, application)` ignores qualifier
        // and organization on Linux and lowercases `application`, which would silently put data
        // under `~/.local/share/audiobookshelf/` instead of the reverse-DNS-named directory this
        // app (and Flatpak) actually expects — confirmed by running it, not assumed. `resolve()`
        // must build the path from `BaseDirs` + the literal `APP_ID`, not `ProjectDirs`.
        if let Some(paths) = AppPaths::resolve() {
            assert_eq!(paths.data_dir().file_name().unwrap(), APP_ID);
            assert_eq!(paths.cache_dir().file_name().unwrap(), APP_ID);
            assert_eq!(paths.state_dir().file_name().unwrap(), APP_ID);
        }
    }
}
