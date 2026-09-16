//! Resolves the on-disk locations this app uses, following the XDG Base Directory layout
//! documented in the architecture plan: durable state under `$XDG_DATA_HOME`, evictable cache
//! under `$XDG_CACHE_HOME`. Nothing outside this module computes one of these paths directly.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// The app's reverse-DNS identity — also the GTK application id and, on Flatpak, the sandbox's
/// per-app data directory name, so this is deliberately used verbatim as the directory name
/// below rather than through `directories::ProjectDirs`. `ProjectDirs::from(qualifier,
/// organization, application)` looks like the natural fit, but on Linux it ignores `qualifier`
/// and `organization` entirely and just lowercases `application` — confirmed by actually running
/// it, not assumed — which would have put this app's data under `~/.local/share/audiobookshelf/`
/// instead of the reverse-DNS-named directory GNOME apps (and Flatpak) actually use.
pub const APP_ID: &str = "io.github.gdr_aislop.Audiobookshelf";

#[derive(Debug, Clone)]
pub struct AppPaths {
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    /// Resolve paths from the real XDG environment. Returns `None` if no home directory can be
    /// determined at all (extremely unusual — `directories` falls back sensibly otherwise).
    pub fn resolve() -> Option<Self> {
        let dirs = BaseDirs::new()?;
        Some(Self {
            data_dir: dirs.data_dir().join(APP_ID),
            cache_dir: dirs.cache_dir().join(APP_ID),
        })
    }

    /// Build an instance rooted at arbitrary directories — used by tests (and could be used for
    /// a future multi-profile mode) instead of touching the real user environment.
    pub fn rooted_at(data_dir: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            cache_dir: cache_dir.into(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
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

    /// Path a given chapter of an item would be downloaded to. `chapter_index` is zero-based;
    /// the on-disk filename is 1-based and zero-padded to sort correctly as plain text.
    pub fn chapter_file_path(
        &self,
        server_id: &str,
        item_id: &str,
        chapter_index: u32,
        title_slug: &str,
    ) -> PathBuf {
        self.item_downloads_dir(server_id, item_id)
            .join(format!("{:03}-{}.mp3", chapter_index + 1, title_slug))
    }

    pub fn covers_dir(&self) -> PathBuf {
        self.cache_dir.join("covers")
    }

    pub fn cover_cache_path(&self, server_id: &str, item_id: &str) -> PathBuf {
        self.covers_dir()
            .join(server_id)
            .join(format!("{item_id}.jpg"))
    }

    /// Create every directory this `AppPaths` might write into. Idempotent.
    pub async fn ensure_dirs(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.data_dir).await?;
        tokio::fs::create_dir_all(self.downloads_dir()).await?;
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        tokio::fs::create_dir_all(self.covers_dir()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let paths = AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
        (tmp, paths)
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
    fn chapter_file_path_is_one_indexed_and_zero_padded() {
        let (_tmp, paths) = test_paths();
        let p0 = paths.chapter_file_path("s", "i", 0, "intro");
        let p9 = paths.chapter_file_path("s", "i", 9, "tenth-chapter");
        assert_eq!(p0.file_name().unwrap(), "001-intro.mp3");
        assert_eq!(p9.file_name().unwrap(), "010-tenth-chapter.mp3");
    }

    #[test]
    fn chapter_file_paths_sort_lexically_in_chapter_order() {
        let (_tmp, paths) = test_paths();
        let names: Vec<String> = (0..12)
            .map(|i| {
                paths
                    .chapter_file_path("s", "i", i, "c")
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        // A naive (unpadded) "1-c.mp3", "10-c.mp3", "2-c.mp3", ... would reorder here; zero
        // padding must keep insertion order == lexical order.
        assert_eq!(names, sorted);
    }

    #[test]
    fn cover_cache_path_lives_under_cache_dir_not_data_dir() {
        let (_tmp, paths) = test_paths();
        let cover = paths.cover_cache_path("server-a", "item-1");
        assert!(cover.starts_with(paths.cache_dir()));
        assert!(!cover.starts_with(paths.data_dir()));
    }

    #[tokio::test]
    async fn ensure_dirs_creates_every_directory_it_promises() {
        let (_tmp, paths) = test_paths();
        paths.ensure_dirs().await.expect("ensure_dirs succeeds");

        assert!(paths.data_dir().is_dir());
        assert!(paths.downloads_dir().is_dir());
        assert!(paths.cache_dir().is_dir());
        assert!(paths.covers_dir().is_dir());
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

    #[test]
    fn resolve_produces_distinct_data_and_cache_dirs() {
        // This touches the real environment (HOME/XDG_*), so just check internal consistency
        // rather than asserting a specific path — the sandbox running this test may not have a
        // conventional home directory.
        if let Some(paths) = AppPaths::resolve() {
            assert_ne!(paths.data_dir(), paths.cache_dir());
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
        }
    }
}
