//! The Library tab's content — a searchable/sortable grid of every item across every synced
//! library, per `docs/design/ui-spec.md`'s "Library browse" section. Unlike Home's two curated
//! 10-item shelves, this shows everything.
//!
//! Deliberately out of scope for this pass (see the implementation plan): the list/grid view
//! toggle, category chips (author/series/genre — this app doesn't model series/genre at all yet),
//! the view-options bottom sheet ("Downloaded only", "Hide finished", "Grouping"), and sticky
//! section headers. Search and sort are client-side over the already-synced local table — neither
//! `abs-storage` nor the real Audiobookshelf API surface this client uses expose search/sort/
//! pagination query params, so there is nothing server-side to delegate to yet.

use std::cell::Cell;
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::error::Result as CoreResult;
use abs_storage::models::{Account, Item, Server};
use abs_storage::AppPaths;

use crate::player::PlayRequest;
use crate::widgets::item_card;

// Small enough that at least 2 columns fit at this app's default phone width (390px, see
// `application.rs`) once the flat button's own padding and the `GtkFlowBoxChild` wrapper's
// intrinsic padding are added on top of the raw cover size — confirmed live: 140 rendered as a
// single column with a lot of unused horizontal space, since the button+wrapper overhead alone was
// enough to push the cell's natural width just past half the available content width.
const TILE_SIZE: i32 = 108;

pub struct LibraryScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub status_page: adw::StatusPage,
    pub flow_box: gtk4::FlowBox,
    pub search_entry: gtk4::SearchEntry,
    pub banner: crate::widgets::banner::ErrorBanner,
    pub sort_buttons: SortButtons,
}

#[cfg(test)]
impl LibraryScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    DateAdded,
    Title,
    Author,
    Duration,
}

#[derive(Clone)]
pub struct SortButtons {
    pub date_added: gtk4::Button,
    pub title: gtk4::Button,
    pub author: gtk4::Button,
    pub duration: gtk4::Button,
}

/// Everything `render` needs a handle to, cloned as a whole into the `spawn_future_local` block —
/// every field is a reference-counted GTK/Adwaita widget handle, so cloning is cheap and shares
/// the same underlying widgets, not copies. Mirrors `home.rs`'s `HomeWidgets` shape.
#[derive(Clone)]
struct LibraryWidgets {
    status_page: adw::StatusPage,
    scroller: gtk4::ScrolledWindow,
    flow_box: gtk4::FlowBox,
    banner: crate::widgets::banner::ErrorBanner,
    search_entry: gtk4::SearchEntry,
    sort: Rc<Cell<SortKey>>,
    data: Rc<std::cell::RefCell<LibraryData>>,
    on_play: Rc<dyn Fn(PlayRequest)>,
}

struct LibraryData {
    items: Vec<Item>,
}

/// Builds the screen. Signature mirrors `home::build`'s exactly — same reasoning: `server`/
/// `account` are already-resolved rows the caller looks up once, and `on_play` is how tapping a
/// cover starts playback without this screen ever touching `abs-player`/`abs-core::streaming`
/// itself.
pub fn build(
    pool: SqlitePool,
    paths: AppPaths,
    server: Server,
    account: Account,
    on_play: impl Fn(PlayRequest) + Clone + 'static,
) -> LibraryScreen {
    let header = adw::HeaderBar::new();

    // A persistent, always-visible search entry — not revealed behind a search button — per the
    // ui-spec's explicit reasoning: search is high-frequency in a large library, worth the
    // permanent header-bar space on a device where reveal-then-tap-then-type is already awkward
    // one-handed.
    let search_entry = gtk4::SearchEntry::builder().hexpand(true).placeholder_text("Search library").build();
    header.set_title_widget(Some(&search_entry));

    let sort_buttons = SortButtons {
        date_added: gtk4::Button::builder().label("Date added").build(),
        title: gtk4::Button::builder().label("Title").build(),
        author: gtk4::Button::builder().label("Author").build(),
        duration: gtk4::Button::builder().label("Duration").build(),
    };
    let sort_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(2).build();
    sort_box.append(&sort_buttons.date_added);
    sort_box.append(&sort_buttons.title);
    sort_box.append(&sort_buttons.author);
    sort_box.append(&sort_buttons.duration);
    let sort_popover = gtk4::Popover::builder().child(&sort_box).build();
    let sort_menu_button = gtk4::MenuButton::builder().icon_name("view-sort-descending-symbolic").tooltip_text("Sort by").popover(&sort_popover).build();
    header.pack_end(&sort_menu_button);

    let banner = crate::widgets::banner::ErrorBanner::new();

    let flow_box = gtk4::FlowBox::builder()
        .homogeneous(true)
        .column_spacing(8)
        .row_spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(16)
        .selection_mode(gtk4::SelectionMode::None)
        .valign(gtk4::Align::Start)
        .build();

    let scroll_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    scroll_content.append(banner.widget());
    scroll_content.append(&flow_box);

    let scroller = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).child(&scroll_content).build();

    let status_page = adw::StatusPage::builder()
        .icon_name("folder-music-symbolic")
        .title("No items yet")
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

    let widgets = LibraryWidgets {
        status_page: status_page.clone(),
        scroller: scroller.clone(),
        flow_box: flow_box.clone(),
        banner: banner.clone(),
        search_entry: search_entry.clone(),
        sort: Rc::new(Cell::new(SortKey::DateAdded)),
        data: Rc::new(std::cell::RefCell::new(LibraryData { items: Vec::new() })),
        on_play: Rc::new(on_play),
    };

    search_entry.connect_search_changed({
        let widgets = widgets.clone();
        move |_| render_from_current_data(&widgets)
    });

    for (button, key) in [
        (&sort_buttons.date_added, SortKey::DateAdded),
        (&sort_buttons.title, SortKey::Title),
        (&sort_buttons.author, SortKey::Author),
        (&sort_buttons.duration, SortKey::Duration),
    ] {
        button.connect_clicked({
            let widgets = widgets.clone();
            let sort_popover = sort_popover.clone();
            move |_| {
                widgets.sort.set(key);
                render_from_current_data(&widgets);
                sort_popover.popdown();
            }
        });
    }

    // Same 3-phase render shape as `home.rs`: render from cache immediately, sync + reconcile
    // progress, re-render, then fetch cover art concurrently for everything just rendered and
    // re-render once more. This is the only place this screen touches `abs_core`; it never
    // imports `abs_api` at all.
    glib::spawn_future_local({
        let pool = pool.clone();
        let paths = paths.clone();
        let widgets = widgets.clone();
        let server_url = server.url.clone();
        let server_id = server.id.clone();
        let account_id = account.id.clone();
        let access_token = account.token.clone();
        async move {
            if let Ok(data) = load(&pool, &server_id).await {
                apply(data, &widgets);
            }

            let sync_result = abs_core::sync::sync_all(&pool, &server_url, &server_id, &access_token).await;

            if let Err(err) = abs_core::progress_sync::reconcile_all_progress(&pool, &server_url, &access_token, &account_id, &server_id).await
            {
                tracing::warn!(%err, "couldn't reconcile progress with the server; showing local progress");
            }

            let mut item_ids_after_sync: Vec<String> = Vec::new();
            if let Ok(data) = load(&pool, &server_id).await {
                item_ids_after_sync = data.iter().map(|item| item.id.clone()).collect();
                apply(data, &widgets);
            }

            // Cover art is cosmetic and best-effort (same posture as Home's own cover fetch),
            // fetched concurrently for everything just rendered.
            if !item_ids_after_sync.is_empty() {
                let fetches = item_ids_after_sync
                    .iter()
                    .map(|item_id| abs_core::covers::fetch_and_cache_cover(&paths, &pool, &server_url, &access_token, &server_id, item_id));
                futures::future::join_all(fetches).await;

                if let Ok(data) = load(&pool, &server_id).await {
                    apply(data, &widgets);
                }
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

    LibraryScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks { status_page, flow_box, search_entry, banner, sort_buttons },
    }
}

/// Reads whatever's currently cached locally across every synced library — never talks to the
/// network. Unlike Home's `load()`, nothing is truncated or pre-sorted here: filtering/sorting for
/// display happens in `render_visible` against the search text and chosen `SortKey`.
async fn load(pool: &SqlitePool, server_id: &str) -> CoreResult<Vec<Item>> {
    let libraries = abs_storage::repo::libraries::list_for_server(pool, server_id).await?;
    let mut items = Vec::new();
    for library in &libraries {
        items.extend(abs_storage::repo::items::list_for_library(pool, server_id, &library.id).await?);
    }
    Ok(items)
}

fn apply(items: Vec<Item>, widgets: &LibraryWidgets) {
    let has_any_item = !items.is_empty();
    widgets.data.borrow_mut().items = items;
    widgets.status_page.set_visible(!has_any_item);
    widgets.scroller.set_visible(has_any_item);
    render_from_current_data(widgets);
}

fn render_from_current_data(widgets: &LibraryWidgets) {
    let query = widgets.search_entry.text().to_lowercase();
    let sort = widgets.sort.get();
    let data = widgets.data.borrow();

    let mut visible: Vec<&Item> = data
        .items
        .iter()
        .filter(|item| {
            query.is_empty()
                || item.title.to_lowercase().contains(&query)
                || item.author.as_deref().is_some_and(|author| author.to_lowercase().contains(&query))
        })
        .collect();

    match sort {
        SortKey::DateAdded => visible.sort_by_key(|item| std::cmp::Reverse(item.added_at)),
        SortKey::Title => visible.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        SortKey::Author => visible.sort_by(|a, b| {
            a.author.as_deref().unwrap_or("").to_lowercase().cmp(&b.author.as_deref().unwrap_or("").to_lowercase())
        }),
        SortKey::Duration => visible.sort_by(|a, b| b.duration_seconds.total_cmp(&a.duration_seconds)),
    }

    clear_flow_box(&widgets.flow_box);
    for item in visible {
        let subtitle = item_subtitle(item);
        widgets.flow_box.insert(&item_card::build(TILE_SIZE, item, &subtitle, &widgets.on_play), -1);
    }
}

fn item_subtitle(item: &Item) -> String {
    let hours = item.duration_seconds / 3600.0;
    format!("{} · {hours:.1}h", item.author.as_deref().unwrap_or("Unknown author"))
}

fn clear_flow_box(fb: &gtk4::FlowBox) {
    while let Some(child) = fb.child_at_index(0) {
        fb.remove(&child);
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

    fn item_json(id: &str, title: &str, author: &str, added_at_ms: i64, duration: f64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "addedAt": added_at_ms,
            "media": {
                "duration": duration,
                "metadata": { "title": title, "authorName": author }
            }
        })
    }

    fn flow_box_titles(flow_box: &gtk4::FlowBox) -> Vec<String> {
        let mut titles = Vec::new();
        let mut index = 0;
        while let Some(child) = flow_box.child_at_index(index) {
            let button = child.child().and_then(|w| w.downcast::<gtk4::Button>().ok()).expect("flow box child wraps a button");
            let card_box = button.child().and_then(|w| w.downcast::<gtk4::Box>().ok()).expect("button wraps the card box");
            let title_label = card_box
                .first_child()
                .and_then(|cover| cover.next_sibling())
                .and_then(|w| w.downcast::<gtk4::Label>().ok())
                .expect("card's second child is the title label");
            titles.push(title_label.text().to_string());
            index += 1;
        }
        titles
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run_renders_all_synced_items(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "libraries": [
                        { "id": "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", "name": "Audiobooks", "mediaType": "book" },
                        { "id": "b1a2c3d4-5e6f-4789-a0b1-c2d3e4f56789", "name": "Podcasts", "mediaType": "podcast" }
                    ]
                })))
                .mount(&mock_server),
        );
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [
                        item_json("item-1", "Project Hail Mary", "Andy Weir", 1_700_000_000_000, 3600.0),
                        item_json("item-2", "The Martian", "Andy Weir", 1_600_000_000_000, 7200.0)
                    ]
                })))
                .mount(&mock_server),
        );
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries/b1a2c3d4-5e6f-4789-a0b1-c2d3e4f56789/items"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [item_json("item-3", "Radiolab Ep 1", "WNYC", 1_650_000_000_000, 1800.0)]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));

        assert!(!hooks.status_page.is_visible(), "should have left the empty state once items synced");
        let mut index = 0;
        while hooks.flow_box.child_at_index(index).is_some() {
            index += 1;
        }
        assert_eq!(index, 3, "all three items across both libraries should render");
    }

    pub(crate) fn run_search_filters_by_title_and_author(runtime: &tokio::runtime::Runtime) {
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
                    "results": [
                        item_json("item-1", "Project Hail Mary", "Andy Weir", 1_700_000_000_000, 3600.0),
                        item_json("item-2", "Dune", "Frank Herbert", 1_600_000_000_000, 7200.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));

        hooks.search_entry.set_text("weir");
        pump_until(|| flow_box_titles(&hooks.flow_box).len() == 1, Duration::from_secs(5));

        let titles = flow_box_titles(&hooks.flow_box);
        assert_eq!(titles, vec!["Project Hail Mary"], "searching an author substring should filter to matching items");

        hooks.search_entry.set_text("dune");
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Dune".to_string()], Duration::from_secs(5));
    }

    pub(crate) fn run_sort_changes_order(runtime: &tokio::runtime::Runtime) {
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
                    "results": [
                        item_json("item-1", "Zed Book", "Author A", 1_700_000_000_000, 3600.0),
                        item_json("item-2", "Alpha Book", "Author B", 1_600_000_000_000, 7200.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));

        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Zed Book", "Alpha Book"], "default sort is date-added descending");

        hooks.sort_buttons.title.emit_clicked();
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Alpha Book".to_string(), "Zed Book".to_string()], Duration::from_secs(5));

        hooks.sort_buttons.date_added.emit_clicked();
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Zed Book".to_string(), "Alpha Book".to_string()], Duration::from_secs(5));
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.banner.widget().reveals_child(), Duration::from_secs(10));

        assert!(hooks.banner.widget().reveals_child(), "a sync failure should show the banner");
        assert!(hooks.status_page.is_visible(), "with nothing cached yet, the empty state stays up too");
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| false, Duration::from_millis(500));

        assert!(hooks.status_page.is_visible(), "no libraries at all should show the empty state");
    }

    pub(crate) fn run_tapping_a_card_invokes_on_play(runtime: &tokio::runtime::Runtime) {
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
                    "results": [item_json("item-1", "Project Hail Mary", "Andy Weir", 1_700_000_000_000, 3600.0)]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let played: Rc<std::cell::RefCell<Vec<PlayRequest>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
        let on_play = {
            let played = played.clone();
            move |request: PlayRequest| played.borrow_mut().push(request)
        };

        let screen = build(pool, crate::test_support::test_paths(), server, account, on_play);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));

        let child = hooks.flow_box.child_at_index(0).unwrap();
        let button = child.child().and_then(|w| w.downcast::<gtk4::Button>().ok()).expect("flow box child wraps a button");
        button.emit_clicked();

        assert_eq!(played.borrow().len(), 1);
        assert_eq!(played.borrow()[0].item_id, "item-1");
    }

    /// End-to-end against the real public demo server, mirroring `welcome.rs`/`home.rs`'s live
    /// test tier. Not a `#[test]` itself — run via `main.rs`'s consolidated `#[ignore]`d entry
    /// point.
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(20));

        assert!(!hooks.status_page.is_visible(), "the live demo server has at least one item, so the empty state should clear");
        assert!(hooks.flow_box.child_at_index(0).is_some());
    }
}
