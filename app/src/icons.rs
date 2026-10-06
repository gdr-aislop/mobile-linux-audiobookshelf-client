//! Icons the app ships itself, compiled into the binary from `app/data/` by `build.rs`.
//!
//! The Adwaita icon theme doesn't have every icon GNOME apps use: `funnel-symbolic` (the Library's
//! "a filter is on" indicator) is Nautilus' own, so with only the system theme (or the Adwaita copy
//! the AppImage bundles) GTK drew its "missing icon" placeholder instead, without logging anything.
//! Icons added here act as part of the hicolor fallback theme, so a theme that has its own version
//! still wins.

/// The Library: no stock Adwaita icon means "library" (the old `system-file-manager-symbolic` is a
/// legacy icon newer Adwaita versions dropped, so the flatpak drew a full-colour one in its place).
/// Named for this app so another theme's "library" icon can't stand in for it.
pub const LIBRARY: &str = "abs-library-symbolic";
/// Settings: a gear. Adwaita's `emblem-system-symbolic` gear is gone from its current set, and
/// `preferences-system-symbolic` is a wrench and screwdriver.
pub const SETTINGS: &str = "abs-settings-symbolic";

/// Where `data/resources.gresource.xml` puts the icon directories.
const ICONS_RESOURCE_PATH: &str = "/io/github/gdr_aislop/abs-app/icons";

/// Makes the bundled icons available on `display`. Safe to call more than once.
pub fn register(display: &gtk4::gdk::Display) {
    static REGISTER_RESOURCES: std::sync::Once = std::sync::Once::new();
    REGISTER_RESOURCES.call_once(|| {
        gtk4::gio::resources_register_include!("abs-app.gresource").expect("the bundled resources are valid");
    });
    let theme = gtk4::IconTheme::for_display(display);
    if !theme.resource_path().iter().any(|path| path == ICONS_RESOURCE_PATH) {
        theme.add_resource_path(ICONS_RESOURCE_PATH);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// Every `"…-symbolic"` icon name in the app's source must be one every supported system has:
    /// in `data/stock-icons.txt` (Adwaita's current icons, in both the oldest and newest version
    /// we run on; see `scripts/update-stock-icons.sh`), bundled in `data/icons/`, or libadwaita's
    /// own (`adw-…`). Otherwise GTK silently draws whatever it finds instead: a full-colour icon
    /// from another set, or a "missing icon" placeholder. Checking the list rather than this
    /// machine's theme matters: its Adwaita still had legacy icons users' systems don't.
    pub(crate) fn run_every_icon_the_app_uses_is_in_the_theme(_rt: &tokio::runtime::Runtime) {
        adw::init().expect("libadwaita registers its own icons on init");
        let display = gtk4::gdk::Display::default().expect("a display");
        super::register(&display);
        let theme = gtk4::IconTheme::for_display(&display);

        fn source_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    source_files(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        let mut files = Vec::new();
        source_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut names = std::collections::BTreeSet::new();
        for file in &files {
            let source = std::fs::read_to_string(file).unwrap();
            for piece in source.split('"') {
                if piece.ends_with("-symbolic") && !piece.starts_with('-') && piece.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                    names.insert(piece.to_string());
                }
            }
        }
        assert!(names.contains("funnel-symbolic"), "the scan should find the icon names in the source");

        let stock: std::collections::HashSet<&str> =
            include_str!("../data/stock-icons.txt").lines().filter(|line| !line.starts_with('#')).collect();
        let bundled: Vec<&str> = include_str!("../data/resources.gresource.xml")
            .lines()
            .filter_map(|line| line.trim().strip_prefix("<file>icons/")?.strip_suffix(".svg</file>"))
            .filter_map(|path| path.rsplit('/').next())
            .collect();
        assert!(bundled.contains(&"funnel-symbolic"), "the bundled icons should be read from the gresource manifest");
        let unsupported: Vec<_> = names
            .iter()
            .filter(|name| !stock.contains(name.as_str()) && !bundled.contains(&name.as_str()) && !name.starts_with("adw-"))
            .collect();
        assert!(unsupported.is_empty(), "icons not in every supported Adwaita (legacy, dropped or never there): {unsupported:?}");

        let missing: Vec<_> = names.iter().filter(|name| !theme.has_icon(name)).collect();
        assert!(missing.is_empty(), "icons the theme doesn't have (GTK would draw a placeholder): {missing:?}");
    }
}
