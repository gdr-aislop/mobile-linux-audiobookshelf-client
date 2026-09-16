//! The Home tab's content — one of `main_window`'s four `AdwViewStack` pages. Shows "Continue
//! Listening"/"Recently Added" shelves and a "Your Libraries" list, backed by data synced via
//! `abs_core::sync::sync_all`. See `docs/design/ui-spec.md`'s "Home" section and the published
//! mockup for the visual design this implements.
//!
//! Deliberately out of scope for this pass (see the implementation plan): real cover-art images
//! (shows a plain title card instead — no `abs-api` coverage for `/api/items/:id/cover` yet), the
//! offline-mode toggle and manual "Sync now" action, and server-side progress sync (so "Continue
//! Listening" only reflects local progress rows, which nothing writes yet without a Player
//! screen — an empty shelf here is the expected, correct state for now, not a bug).

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::error::Result as CoreResult;
use abs_storage::models::{Account, Item, Library, Progress, Server};

pub struct HomeScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub status_page: adw::StatusPage,
    pub libraries_list: gtk4::ListBox,
    pub continue_section: gtk4::Box,
    pub recent_row: gtk4::Box,
    pub banner: crate::widgets::banner::ErrorBanner,
}

#[cfg(test)]
impl HomeScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

/// Everything `apply` needs a handle to, cloned as a whole into the `spawn_future_local` block —
/// every field is a reference-counted GTK/Adwaita widget handle, so cloning is cheap and shares
/// the same underlying widgets, not copies.
#[derive(Clone)]
struct HomeWidgets {
    status_page: adw::StatusPage,
    scroller: gtk4::ScrolledWindow,
    continue_section: gtk4::Box,
    continue_row: gtk4::Box,
    recent_row: gtk4::Box,
    libraries_list: gtk4::ListBox,
    banner: crate::widgets::banner::ErrorBanner,
}

struct HomeData {
    libraries: Vec<Library>,
    recent_items: Vec<Item>,
    continue_items: Vec<(Item, Progress)>,
}

/// Builds the screen. `server`/`account` are already-resolved rows (the caller, `main_window`,
/// looks them up once) rather than bare ids, so this module never has to fail on a missing
/// server/account — that would be a caller bug, not a Home-screen concern.
pub fn build(pool: SqlitePool, server: Server, account: Account) -> HomeScreen {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Home", "")));

    let avatar_letter = account
        .username
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let avatar = gtk4::Button::builder()
        .css_classes(["circular", "suggested-action"])
        .valign(gtk4::Align::Center)
        .child(&gtk4::Label::new(Some(&avatar_letter)))
        .build();
    header.pack_end(&avatar);

    let banner = crate::widgets::banner::ErrorBanner::new();

    let continue_row = shelf_row();
    let continue_section = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .visible(false)
        .build();
    continue_section.append(&section_heading("Continue Listening"));
    continue_section.append(&shelf_scroller(&continue_row));

    let recent_row = shelf_row();
    let recent_section = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    recent_section.append(&section_heading("Recently Added"));
    recent_section.append(&shelf_scroller(&recent_row));

    let libraries_list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(16)
        .margin_end(16)
        .margin_top(2)
        .margin_bottom(18)
        .build();
    let libraries_section = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    libraries_section.append(&section_heading("Your Libraries"));
    libraries_section.append(&libraries_list);

    let scroll_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    scroll_content.append(banner.widget());
    scroll_content.append(&continue_section);
    scroll_content.append(&recent_section);
    scroll_content.append(&libraries_section);

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&scroll_content)
        .build();

    let status_page = adw::StatusPage::builder()
        .icon_name("folder-music-symbolic")
        .title("No library synced yet")
        .description("Check your connection and try again.")
        .vexpand(true)
        .visible(false)
        .build();

    let body = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).vexpand(true).build();
    body.append(&scroller);
    body.append(&status_page);

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&body);

    let widgets = HomeWidgets {
        status_page: status_page.clone(),
        scroller: scroller.clone(),
        continue_section: continue_section.clone(),
        continue_row: continue_row.clone(),
        recent_row: recent_row.clone(),
        libraries_list: libraries_list.clone(),
        banner: banner.clone(),
    };

    // Render once immediately from whatever's already cached locally (so a returning session
    // isn't blocked on network), then sync and re-render from local storage again regardless of
    // whether the sync fully succeeded — a partial failure (e.g. libraries synced fine but one
    // library's items didn't) should still show whatever did land rather than discarding it, with
    // the banner surfaced separately. This is the only place this screen touches `abs_core`; it
    // never imports `abs_api` at all.
    glib::spawn_future_local({
        let pool = pool.clone();
        let widgets = widgets.clone();
        let server_url = server.url.clone();
        let server_id = server.id.clone();
        let account_id = account.id.clone();
        let access_token = account.token.clone();
        async move {
            if let Ok(data) = load(&pool, &server_id, &account_id).await {
                apply(&data, &widgets);
            }

            let sync_result = abs_core::sync::sync_all(&pool, &server_url, &server_id, &access_token).await;

            if let Ok(data) = load(&pool, &server_id, &account_id).await {
                apply(&data, &widgets);
            }

            match sync_result {
                Ok(()) => widgets.banner.set_revealed(false),
                Err(err) => {
                    widgets.banner.set_title("Couldn't sync — showing what's cached.");
                    widgets.banner.set_details(Some(&err.to_string()));
                    widgets.banner.set_revealed(true);
                }
            }
        }
    });

    HomeScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks {
            status_page,
            libraries_list,
            continue_section,
            recent_row,
            banner,
        },
    }
}

/// Reads whatever's currently cached locally — never talks to the network. Called once before
/// syncing (to show cached data immediately) and once after (to pick up whatever the sync just
/// wrote).
async fn load(pool: &SqlitePool, server_id: &str, account_id: &str) -> CoreResult<HomeData> {
    let libraries = abs_storage::repo::libraries::list_for_server(pool, server_id).await?;

    let mut recent_items = Vec::new();
    for library in &libraries {
        recent_items.extend(abs_storage::repo::items::list_for_library(pool, server_id, &library.id).await?);
    }
    recent_items.sort_by(|a, b| b.added_at.cmp(&a.added_at));
    recent_items.truncate(10);

    let mut continue_items = Vec::new();
    for progress in abs_storage::repo::progress::list_recent_for_account(pool, account_id, 10).await? {
        // A progress row can outlive the item it points at (e.g. removed from the server between
        // syncs) — skip it rather than failing the whole Home screen over one stale row.
        if let Ok(item) = abs_storage::repo::items::get(pool, server_id, &progress.item_id).await {
            continue_items.push((item, progress));
        }
    }

    Ok(HomeData { libraries, recent_items, continue_items })
}

/// Rebuilds every dynamic widget from `data`. Safe to call repeatedly — clears each container's
/// children first, so this is a full re-render rather than an incremental diff (fine at this
/// scale: a handful of shelf cards and library rows, not a large list needing virtualization).
fn apply(data: &HomeData, widgets: &HomeWidgets) {
    let has_any_library = !data.libraries.is_empty();
    widgets.status_page.set_visible(!has_any_library);
    widgets.scroller.set_visible(has_any_library);

    clear_box(&widgets.continue_row);
    for (item, progress) in &data.continue_items {
        let percent = if item.duration_seconds > 0.0 {
            (progress.current_time_seconds / item.duration_seconds * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let subtitle = format!("{} · {percent:.0}% listened", item.author.as_deref().unwrap_or("Unknown author"));
        widgets.continue_row.append(&cover_card(&item.title, &subtitle));
    }
    widgets.continue_section.set_visible(!data.continue_items.is_empty());

    clear_box(&widgets.recent_row);
    for item in &data.recent_items {
        widgets.recent_row.append(&cover_card(&item.title, &item_subtitle(item)));
    }

    clear_listbox(&widgets.libraries_list);
    for library in &data.libraries {
        widgets.libraries_list.append(&library_row(library));
    }
}

fn item_subtitle(item: &Item) -> String {
    let hours = item.duration_seconds / 3600.0;
    format!("{} · {hours:.1}h", item.author.as_deref().unwrap_or("Unknown author"))
}

fn section_heading(text: &str) -> gtk4::Label {
    gtk4::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_start(16)
        .margin_end(16)
        .margin_top(18)
        .margin_bottom(8)
        .build()
}

fn shelf_row() -> gtk4::Box {
    gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(4)
        .build()
}

fn shelf_scroller(row: &gtk4::Box) -> gtk4::ScrolledWindow {
    gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .child(row)
        .build()
}

/// No real cover-art image (see the module doc) — a plain title-plate card, matching the
/// mockups' own placeholder treatment for items without artwork.
fn cover_card(title: &str, subtitle: &str) -> gtk4::Box {
    let card = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).width_request(132).spacing(6).build();

    let plate = gtk4::Box::builder()
        .css_classes(["card"])
        .width_request(132)
        .height_request(132)
        .valign(gtk4::Align::Center)
        .build();
    let plate_label = gtk4::Label::builder()
        .label(title)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .hexpand(true)
        .margin_start(8)
        .margin_end(8)
        .css_classes(["heading"])
        .build();
    plate.append(&plate_label);

    let meta = gtk4::Label::builder()
        .label(subtitle)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build();

    card.append(&plate);
    card.append(&meta);
    card
}

fn library_row(library: &Library) -> adw::ActionRow {
    let icon_name = if library.media_type == "podcast" { "microphone-symbolic" } else { "system-file-manager-symbolic" };
    let row = adw::ActionRow::builder().title(library.name.as_str()).subtitle(library.media_type.as_str()).build();
    row.add_prefix(&gtk4::Image::from_icon_name(icon_name));
    row
}

fn clear_box(b: &gtk4::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

fn clear_listbox(lb: &gtk4::ListBox) {
    while let Some(child) = lb.row_at_index(0) {
        lb.remove(&child);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn account_and_server(pool: &SqlitePool, server_url: &str) -> (Server, Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123").await.unwrap();
        (
            abs_storage::repo::servers::get(pool, &server_id).await.unwrap(),
            abs_storage::repo::accounts::get(pool, &account_id).await.unwrap(),
        )
    }

    fn item_json(id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "addedAt": 1_700_000_000_000i64,
            "media": {
                "duration": 3600.0,
                "metadata": { "title": title, "authorName": "Andy Weir" }
            }
        })
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run_renders_synced_library_and_recently_added_item(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "libraries": [{ "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", "name": "Audiobooks", "mediaType": "book" }]
                })))
                .mount(&mock_server),
        );
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [item_json("item-1", "Project Hail Mary")]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, server, account);
        let hooks = screen.test_hooks();

        // `status_page` starts hidden (only shown for a confirmed-empty result), so waiting on
        // its visibility can't distinguish "sync hasn't run yet" from "sync ran and found
        // nothing" — wait on the actual data landing instead.
        pump_until(|| hooks.libraries_list.row_at_index(0).is_some(), Duration::from_secs(10));

        assert!(!hooks.status_page.is_visible(), "should have left the empty state once a library synced");
        assert!(hooks.libraries_list.row_at_index(0).is_some(), "the synced library should have a row");
        assert!(hooks.recent_row.first_child().is_some(), "the synced item should show under Recently Added");
        assert!(!hooks.continue_section.is_visible(), "no progress exists yet, so Continue Listening stays hidden");
    }

    pub(crate) fn run_shows_empty_state_when_the_server_has_no_libraries(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "libraries": [] })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, server, account);
        let hooks = screen.test_hooks();

        // There's no "sync finished" signal to await directly here (an always-empty result looks
        // identical before and after sync), so just give the spawned future time to run.
        pump_until(|| false, Duration::from_millis(500));

        assert!(hooks.status_page.is_visible(), "no libraries at all should show the empty state");
    }

    pub(crate) fn run_shows_a_banner_when_sync_fails(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, server, account);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.banner.widget().reveals_child(), Duration::from_secs(10));

        assert!(hooks.banner.widget().reveals_child(), "a sync failure should show the banner");
        assert!(hooks.status_page.is_visible(), "with nothing cached yet, the empty state stays up too");
    }

    /// End-to-end against the real public demo server, mirroring `welcome.rs`'s live test tier.
    /// Not a `#[test]` itself — run via `main.rs`'s consolidated `#[ignore]`d entry point.
    pub(crate) fn run_live(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let added = runtime
            .block_on(abs_core::accounts::add_server_and_login(
                &pool,
                "https://audiobooks.dev/audiobookshelf",
                "demo",
                "demo",
            ))
            .expect("login against the live demo server should succeed");
        let server = runtime.block_on(abs_storage::repo::servers::get(&pool, &added.server_id)).unwrap();
        let account = runtime.block_on(abs_storage::repo::accounts::get(&pool, &added.account_id)).unwrap();

        let screen = build(pool, server, account);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.libraries_list.row_at_index(0).is_some(), Duration::from_secs(20));

        assert!(
            !hooks.status_page.is_visible(),
            "the live demo server has at least one library, so the empty state should clear"
        );
        assert!(hooks.libraries_list.row_at_index(0).is_some());
    }
}
