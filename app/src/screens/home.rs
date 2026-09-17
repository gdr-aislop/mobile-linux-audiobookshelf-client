//! The Home tab's content — one of `main_window`'s four `AdwViewStack` pages. Shows "Continue
//! Listening"/"Recently Added" shelves and a "Your Libraries" list, backed by data synced via
//! `abs_core::sync::sync_all`. See `docs/design/ui-spec.md`'s "Home" section and the published
//! mockup for the visual design this implements.
//!
//! Deliberately out of scope for this pass (see the implementation plan): the offline-mode toggle
//! and manual "Sync now" action. Cover art now reuses the same `abs_core::covers`/`CoverImage`
//! pipeline the player screen already built.

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::error::Result as CoreResult;
use abs_storage::models::{Account, Item, Library, Progress, Server};
use abs_storage::AppPaths;

use crate::player::PlayRequest;
use crate::widgets::item_card;

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
    on_play: std::rc::Rc<dyn Fn(PlayRequest)>,
}

struct HomeData {
    libraries: Vec<Library>,
    recent_items: Vec<Item>,
    continue_items: Vec<(Item, Progress)>,
}

/// Builds the screen. `server`/`account` are already-resolved rows (the caller, `main_window`,
/// looks them up once) rather than bare ids, so this module never has to fail on a missing
/// server/account — that would be a caller bug, not a Home-screen concern. `on_play` is how
/// tapping a cover card starts playback — Home never touches `abs-player`/`abs-core::streaming`
/// itself, matching the `on_success`-callback pattern `welcome.rs` already uses.
pub fn build(
    pool: SqlitePool,
    paths: AppPaths,
    server: Server,
    account: Account,
    on_play: impl Fn(PlayRequest) + Clone + 'static,
) -> HomeScreen {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Home", "")));

    let avatar_letter = account
        .username
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    // There's no account-switcher/sign-out screen built yet (per `docs/design/ui-spec.md`, that
    // lives in Settings' Servers group, still a stub tab) — until it exists, this button can't
    // actually do anything, so a tooltip is the honest fix: it should read as "this is who I'm
    // signed in as", not as a dead button whose purpose is a mystery.
    let avatar = gtk4::Button::builder()
        .css_classes(["circular", "suggested-action"])
        .valign(gtk4::Align::Center)
        .tooltip_text(format!("Signed in as {}", account.username))
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
        on_play: std::rc::Rc::new(on_play),
    };

    // Render once immediately from whatever's already cached locally (so a returning session
    // isn't blocked on network), then sync and re-render from local storage again regardless of
    // whether the sync fully succeeded — a partial failure (e.g. libraries synced fine but one
    // library's items didn't) should still show whatever did land rather than discarding it, with
    // the banner surfaced separately. This is the only place this screen touches `abs_core`; it
    // never imports `abs_api` at all.
    //
    // The network/DB pipeline runs inside `tokio::spawn` (worker threads), not directly in this
    // `spawn_future_local` future: the latter is polled on the GTK main thread, so HTTP body
    // chunk handling, statement building and cache-file writes done directly here steal frames
    // from the main loop — visibly, when dozens of per-item cover fetches all poll at once
    // (observed as a multi-second UI hang while scrolling the Continue Listening shelf). The
    // main context only parks on the `JoinHandle`s, which costs nothing to await, and widget
    // updates happen back on this thread between stages.
    glib::spawn_future_local({
        let pool = pool.clone();
        let widgets = widgets.clone();
        let server_id = server.id.clone();
        let account_id = account.id.clone();
        async move {
            if let Ok(data) = load(&pool, &server_id, &account_id).await {
                apply(&data, &widgets);
            }

            let spawned_sync = tokio::spawn({
                let pool = pool.clone();
                let server_url = server.url.clone();
                let server_id = server_id.clone();
                let account_id = account_id.clone();
                let access_token = account.token.clone();
                async move {
                    let sync_result = abs_core::sync::sync_all(&pool, &server_url, &server_id, &access_token).await;

                    // Reconciling "Continue Listening" against the server's progress runs after
                    // sync_all, not concurrently with it: an item's progress can only be attached
                    // once the item itself has been synced locally (a fresh login has no local
                    // items at all yet). It's still best-effort and bounded by its own short
                    // timeout — a failure here (offline, slow connection) is logged and never
                    // surfaced as this screen's sync banner, which is about library/item sync,
                    // not this.
                    if let Err(err) = abs_core::progress_sync::reconcile_all_progress(&pool, &server_url, &access_token, &account_id, &server_id).await
                    {
                        tracing::warn!(%err, "couldn't reconcile Continue Listening progress with the server; showing local progress");
                    }

                    let data_after_sync = load(&pool, &server_id, &account_id).await.ok();
                    (sync_result, data_after_sync)
                }
            });
            let (sync_result, data_after_sync) = spawned_sync
                .await
                .expect("the Home sync task must not panic");
            if let Some(data) = &data_after_sync {
                apply(data, &widgets);
            }

            // Cover art is cosmetic and best-effort (same posture as `abs_core::covers` already
            // uses for the player screen) — fetched concurrently for every item just rendered,
            // after the rest of the screen is already showing, so a slow/offline server delays
            // only the artwork, never the initial render. `fetch_and_cache_cover` itself no-ops
            // once a cover is already cached on disk, so this is cheap on every subsequent visit.
            let spawned_covers = data_after_sync.as_ref().map(|data| {
                let item_ids: std::collections::BTreeSet<String> = data
                    .recent_items
                    .iter()
                    .map(|item| item.id.clone())
                    .chain(data.continue_items.iter().map(|(item, _)| item.id.clone()))
                    .collect();
                tokio::spawn({
                    let pool = pool.clone();
                    let paths = paths.clone();
                    let server_url = server.url.clone();
                    let server_id = server_id.clone();
                    let account_id = account_id.clone();
                    let access_token = account.token.clone();
                    async move {
                        let fetches = item_ids.into_iter().map(|item_id| {
                            let pool = pool.clone();
                            let paths = paths.clone();
                            let server_url = server_url.clone();
                            let access_token = access_token.clone();
                            let server_id = server_id.clone();
                            async move {
                                abs_core::covers::fetch_and_cache_cover(&paths, &pool, &server_url, &access_token, &server_id, &item_id).await
                            }
                        });
                        futures::future::join_all(fetches).await;

                        load(&pool, &server_id, &account_id).await.ok()
                    }
                })
            });
            if let Some(spawned_covers) = spawned_covers {
                let data_after_covers = spawned_covers.await.expect("the Home cover-fetch task must not panic");
                if let Some(data) = data_after_covers {
                    apply(&data, &widgets);
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
    recent_items.sort_by_key(|a| std::cmp::Reverse(a.added_at));
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
        widgets.continue_row.append(&item_card::build(132, item, &subtitle, &widgets.on_play, false));
    }
    widgets.continue_section.set_visible(!data.continue_items.is_empty());

    clear_box(&widgets.recent_row);
    for item in &data.recent_items {
        let subtitle = item_subtitle(item);
        widgets.recent_row.append(&item_card::build(132, item, &subtitle, &widgets.on_play, false));
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
        // Without this, a `GtkScrolledWindow` with vertical scrolling disabled still only
        // requests its own default minimum height, not its child's actual (wrapped-label-and-all)
        // natural height — the row's second line of text (author/duration) was getting clipped
        // at the bottom of the shelf rather than being fully shown. This tells it to size to fit.
        .propagate_natural_height(true)
        .child(row)
        .build()
}

fn library_row(library: &Library) -> adw::ActionRow {
    let icon_name = if library.media_type == "podcast" { "audio-input-microphone-symbolic" } else { "system-file-manager-symbolic" };
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
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
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

        let screen = build(pool, crate::test_support::test_paths(), server, account, |_| {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.libraries_list.row_at_index(0).is_some(), Duration::from_secs(20));

        assert!(
            !hooks.status_page.is_visible(),
            "the live demo server has at least one library, so the empty state should clear"
        );
        assert!(hooks.libraries_list.row_at_index(0).is_some());
    }
}
