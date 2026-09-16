//! Typed wrappers over `abs-storage`'s generic settings key/value store. Keys are namespaced
//! strings (`"playback.default_speed"`, ...) chosen here, not in the storage layer, so this is
//! the one place that needs to change if a setting's key or encoding changes. Every load falls
//! back to a sensible default rather than erroring on a missing or corrupt value — settings are
//! never load-bearing enough to fail startup over.

use abs_storage::repo::settings as kv;
use abs_storage::StorageError;
use sqlx::SqlitePool;

use crate::playback::DEFAULT_SPEED;

type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSettings {
    pub default_speed: f64,
    pub skip_back_seconds: i64,
    pub skip_forward_seconds: i64,
    pub sleep_timer_default_minutes: i64,
    pub wifi_only_downloads: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            default_speed: DEFAULT_SPEED,
            skip_back_seconds: 15,
            skip_forward_seconds: 30,
            sleep_timer_default_minutes: 30,
            wifi_only_downloads: true,
        }
    }
}

mod keys {
    pub const DEFAULT_SPEED: &str = "playback.default_speed";
    pub const SKIP_BACK_SECONDS: &str = "playback.skip_back_seconds";
    pub const SKIP_FORWARD_SECONDS: &str = "playback.skip_forward_seconds";
    pub const SLEEP_TIMER_DEFAULT_MINUTES: &str = "playback.sleep_timer_default_minutes";
    pub const WIFI_ONLY_DOWNLOADS: &str = "playback.wifi_only_downloads";
    pub const THEME: &str = "appearance.theme";
    pub const DOWNLOADED_ONLY: &str = "library.downloaded_only";
    pub const HIDE_FINISHED: &str = "library.hide_finished";
    pub const GROUPING: &str = "library.grouping";
    pub const SORT_BY: &str = "library.sort_by";
}

/// Parse a stored value, falling back to `default` (and logging) on a missing key or a value
/// that no longer parses — e.g. after a settings format change in a future version.
async fn parse_or_default<T: std::str::FromStr>(pool: &SqlitePool, key: &str, default: T) -> Result<T> {
    match kv::get(pool, key).await? {
        None => Ok(default),
        Some(raw) => Ok(raw.parse().unwrap_or_else(|_| {
            tracing::warn!(key, raw, "failed to parse stored setting, using default");
            default
        })),
    }
}

pub async fn load_playback_settings(pool: &SqlitePool) -> Result<PlaybackSettings> {
    let defaults = PlaybackSettings::default();
    Ok(PlaybackSettings {
        default_speed: parse_or_default(pool, keys::DEFAULT_SPEED, defaults.default_speed).await?,
        skip_back_seconds: parse_or_default(pool, keys::SKIP_BACK_SECONDS, defaults.skip_back_seconds).await?,
        skip_forward_seconds: parse_or_default(pool, keys::SKIP_FORWARD_SECONDS, defaults.skip_forward_seconds).await?,
        sleep_timer_default_minutes: parse_or_default(
            pool,
            keys::SLEEP_TIMER_DEFAULT_MINUTES,
            defaults.sleep_timer_default_minutes,
        )
        .await?,
        wifi_only_downloads: parse_or_default(pool, keys::WIFI_ONLY_DOWNLOADS, defaults.wifi_only_downloads).await?,
    })
}

pub async fn save_playback_settings(pool: &SqlitePool, settings: &PlaybackSettings) -> Result<()> {
    kv::set(pool, keys::DEFAULT_SPEED, &settings.default_speed.to_string()).await?;
    kv::set(pool, keys::SKIP_BACK_SECONDS, &settings.skip_back_seconds.to_string()).await?;
    kv::set(pool, keys::SKIP_FORWARD_SECONDS, &settings.skip_forward_seconds.to_string()).await?;
    kv::set(
        pool,
        keys::SLEEP_TIMER_DEFAULT_MINUTES,
        &settings.sleep_timer_default_minutes.to_string(),
    )
    .await?;
    kv::set(pool, keys::WIFI_ONLY_DOWNLOADS, &settings.wifi_only_downloads.to_string()).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl std::str::FromStr for Theme {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "system" => Ok(Theme::System),
            "light" => Ok(Theme::Light),
            "dark" => Ok(Theme::Dark),
            _ => Err(()),
        }
    }
}

impl Theme {
    fn as_str(&self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

pub async fn load_theme(pool: &SqlitePool) -> Result<Theme> {
    parse_or_default(pool, keys::THEME, Theme::default()).await
}

pub async fn save_theme(pool: &SqlitePool, theme: Theme) -> Result<()> {
    kv::set(pool, keys::THEME, theme.as_str()).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    None,
    BySeries,
    ByAuthor,
}

impl std::str::FromStr for Grouping {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "none" => Ok(Grouping::None),
            "by_series" => Ok(Grouping::BySeries),
            "by_author" => Ok(Grouping::ByAuthor),
            _ => Err(()),
        }
    }
}

impl Grouping {
    fn as_str(&self) -> &'static str {
        match self {
            Grouping::None => "none",
            Grouping::BySeries => "by_series",
            Grouping::ByAuthor => "by_author",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    DateOfCreation,
    Title,
    Author,
    Duration,
}

impl std::str::FromStr for SortBy {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "date_of_creation" => Ok(SortBy::DateOfCreation),
            "title" => Ok(SortBy::Title),
            "author" => Ok(SortBy::Author),
            "duration" => Ok(SortBy::Duration),
            _ => Err(()),
        }
    }
}

impl SortBy {
    fn as_str(&self) -> &'static str {
        match self {
            SortBy::DateOfCreation => "date_of_creation",
            SortBy::Title => "title",
            SortBy::Author => "author",
            SortBy::Duration => "duration",
        }
    }
}

/// The Library browse screen's view-options sheet: Downloaded only, Hide finished, Grouping,
/// Sort by (see `docs/design/ui-spec.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LibraryViewOptions {
    pub downloaded_only: bool,
    pub hide_finished: bool,
    pub grouping: Grouping,
    pub sort_by: SortBy,
}

pub async fn load_library_view_options(pool: &SqlitePool) -> Result<LibraryViewOptions> {
    Ok(LibraryViewOptions {
        downloaded_only: parse_or_default(pool, keys::DOWNLOADED_ONLY, false).await?,
        hide_finished: parse_or_default(pool, keys::HIDE_FINISHED, false).await?,
        grouping: parse_or_default(pool, keys::GROUPING, Grouping::default()).await?,
        sort_by: parse_or_default(pool, keys::SORT_BY, SortBy::default()).await?,
    })
}

pub async fn save_library_view_options(pool: &SqlitePool, options: &LibraryViewOptions) -> Result<()> {
    kv::set(pool, keys::DOWNLOADED_ONLY, &options.downloaded_only.to_string()).await?;
    kv::set(pool, keys::HIDE_FINISHED, &options.hide_finished.to_string()).await?;
    kv::set(pool, keys::GROUPING, options.grouping.as_str()).await?;
    kv::set(pool, keys::SORT_BY, options.sort_by.as_str()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abs_storage::connect_and_migrate;

    async fn pool() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        let pool = connect_and_migrate(&tmp.path().join("db.sqlite3")).await.unwrap();
        std::mem::forget(tmp);
        pool
    }

    #[tokio::test]
    async fn playback_settings_default_when_nothing_stored() {
        let pool = pool().await;
        assert_eq!(load_playback_settings(&pool).await.unwrap(), PlaybackSettings::default());
    }

    #[tokio::test]
    async fn playback_settings_round_trip() {
        let pool = pool().await;
        let settings = PlaybackSettings {
            default_speed: 1.5,
            skip_back_seconds: 10,
            skip_forward_seconds: 60,
            sleep_timer_default_minutes: 15,
            wifi_only_downloads: false,
        };
        save_playback_settings(&pool, &settings).await.unwrap();
        assert_eq!(load_playback_settings(&pool).await.unwrap(), settings);
    }

    #[tokio::test]
    async fn playback_settings_falls_back_to_default_on_corrupt_value() {
        let pool = pool().await;
        kv::set(&pool, "playback.default_speed", "not-a-number").await.unwrap();
        assert_eq!(
            load_playback_settings(&pool).await.unwrap().default_speed,
            PlaybackSettings::default().default_speed
        );
    }

    #[tokio::test]
    async fn theme_defaults_to_system() {
        let pool = pool().await;
        assert_eq!(load_theme(&pool).await.unwrap(), Theme::System);
    }

    #[tokio::test]
    async fn theme_round_trips_each_variant() {
        let pool = pool().await;
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            save_theme(&pool, theme).await.unwrap();
            assert_eq!(load_theme(&pool).await.unwrap(), theme);
        }
    }

    #[tokio::test]
    async fn theme_falls_back_to_default_on_unrecognized_value() {
        let pool = pool().await;
        kv::set(&pool, "appearance.theme", "solarized").await.unwrap();
        assert_eq!(load_theme(&pool).await.unwrap(), Theme::System);
    }

    #[tokio::test]
    async fn library_view_options_default_to_showing_everything() {
        let pool = pool().await;
        let options = load_library_view_options(&pool).await.unwrap();
        assert!(!options.downloaded_only);
        assert!(!options.hide_finished);
        assert_eq!(options.grouping, Grouping::None);
        assert_eq!(options.sort_by, SortBy::DateOfCreation);
    }

    #[tokio::test]
    async fn library_view_options_round_trip() {
        let pool = pool().await;
        let options = LibraryViewOptions {
            downloaded_only: true,
            hide_finished: true,
            grouping: Grouping::BySeries,
            sort_by: SortBy::Title,
        };
        save_library_view_options(&pool, &options).await.unwrap();
        assert_eq!(load_library_view_options(&pool).await.unwrap(), options);
    }

    #[tokio::test]
    async fn grouping_and_sort_by_round_trip_every_variant() {
        for g in [Grouping::None, Grouping::BySeries, Grouping::ByAuthor] {
            assert_eq!(g.as_str().parse::<Grouping>().unwrap(), g);
        }
        for s in [SortBy::DateOfCreation, SortBy::Title, SortBy::Author, SortBy::Duration] {
            assert_eq!(s.as_str().parse::<SortBy>().unwrap(), s);
        }
    }
}
