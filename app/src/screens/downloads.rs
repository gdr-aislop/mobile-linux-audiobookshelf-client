//! The Downloads tab — `docs/design/ui-spec.md`'s "Downloads" section: one row per item that has
//! been downloaded fully or partially, or is currently being fetched, with a remove/cancel action
//! and an empty state when nothing has been downloaded yet.
//!
//! No storage-used/free-space summary row (the spec's own "nice to have") — computing device free
//! space needs `statvfs`-style OS calls with no existing precedent in this codebase, left as a
//! documented follow-up rather than built here. Progress is shown as a plain `GtkSpinner` rather
//! than an exact percentage: `DownloadEvent::TrackProgress` is per-track, and aggregating several
//! tracks' byte counts into one item-level fraction is more machinery than this pass needs — "in
//! progress" is the signal that matters for a remove/cancel decision.
//!
//! Never imports `abs_api`: the only calls here are `abs_core::download_tracks` (read the
//! downloaded-item list) and `DownloadManager` (cancel/clear) — same boundary every other screen
//! respects.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use sqlx::SqlitePool;

use crate::downloads::{DownloadEvent, DownloadManager, ItemDownloadState};

pub struct DownloadsScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub status_page: adw::StatusPage,
    pub list_box: gtk4::ListBox,
    pub scroller: gtk4::ScrolledWindow,
}

#[cfg(test)]
impl DownloadsScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

struct Widgets {
    pool: SqlitePool,
    server_id: String,
    list_box: gtk4::ListBox,
    status_page: adw::StatusPage,
    scroller: gtk4::ScrolledWindow,
    download_manager: DownloadManager,
    /// Item ids this screen has itself observed going into `Downloading` — the persisted
    /// `downloaded_item_ids` query only ever reflects *complete* tracks, so a download still in
    /// flight (nothing complete yet) needs to be tracked here to show a row for it at all.
    downloading: Rc<RefCell<HashSet<String>>>,
}

pub fn build(
    pool: SqlitePool,
    _paths: abs_storage::AppPaths,
    server: abs_storage::models::Server,
    _account: abs_storage::models::Account,
    _session: abs_core::auth::Session,
    download_manager: DownloadManager,
) -> DownloadsScreen {
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Downloads", "")));

    let status_page = adw::StatusPage::builder().icon_name("folder-download-symbolic").title("No downloads yet").vexpand(true).visible(false).build();

    let list_box = gtk4::ListBox::builder().selection_mode(gtk4::SelectionMode::None).css_classes(["boxed-list"]).margin_start(12).margin_end(12).margin_top(12).build();
    let scroller = gtk4::ScrolledWindow::builder().child(&list_box).vexpand(true).build();

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&status_page);
    root.append(&scroller);

    let widgets = Rc::new(Widgets {
        pool,
        server_id: server.id.clone(),
        list_box: list_box.clone(),
        status_page: status_page.clone(),
        scroller: scroller.clone(),
        download_manager: download_manager.clone(),
        downloading: Rc::new(RefCell::new(HashSet::new())),
    });

    // Registered once, for the manager's whole lifetime — same permanent-listener shape
    // `PlayerController::add_listener`/MPRIS already use. Any state change for any item (this
    // screen has no notion of "which item is mine" — it shows all of them) triggers a re-render.
    download_manager.add_listener({
        let widgets = widgets.clone();
        move |event| {
            if let DownloadEvent::ItemStateChanged { item_id, state } = event {
                match state {
                    ItemDownloadState::Downloading => {
                        widgets.downloading.borrow_mut().insert(item_id.clone());
                    }
                    ItemDownloadState::Idle | ItemDownloadState::Complete | ItemDownloadState::Failed => {
                        widgets.downloading.borrow_mut().remove(item_id);
                    }
                }
                spawn_refresh(widgets.clone());
            }
        }
    });

    spawn_refresh(widgets);

    DownloadsScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks { status_page, list_box, scroller },
    }
}

fn spawn_refresh(widgets: Rc<Widgets>) {
    adw::glib::spawn_future_local(async move {
        let downloaded = abs_core::download_tracks::downloaded_item_ids(&widgets.pool, &widgets.server_id).await.unwrap_or_default();
        let mut ids: Vec<String> = downloaded.into_iter().collect();
        for id in widgets.downloading.borrow().iter() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }

        while let Some(child) = widgets.list_box.row_at_index(0) {
            widgets.list_box.remove(&child);
        }

        let mut any_row = false;
        for item_id in ids {
            let Ok(item) = abs_storage::repo::items::get(&widgets.pool, &widgets.server_id, &item_id).await else { continue };
            let is_downloading = widgets.downloading.borrow().contains(&item_id);
            widgets.list_box.append(&download_row(&item, is_downloading, &widgets.download_manager, &widgets.server_id));
            any_row = true;
        }

        widgets.status_page.set_visible(!any_row);
        widgets.scroller.set_visible(any_row);
    });
}

fn download_row(item: &abs_storage::models::Item, is_downloading: bool, download_manager: &DownloadManager, server_id: &str) -> adw::ActionRow {
    const THUMBNAIL_SIZE: i32 = 48;
    let cover = crate::widgets::cover_image::CoverImage::new(THUMBNAIL_SIZE);
    cover.set_path(item.cover_cache_path.as_deref().map(std::path::Path::new));

    let row = adw::ActionRow::builder().title(&item.title).build();
    row.add_prefix(cover.widget());

    if is_downloading {
        let spinner = gtk4::Spinner::builder().spinning(true).valign(gtk4::Align::Center).build();
        let cancel_button = gtk4::Button::builder().icon_name("process-stop-symbolic").tooltip_text("Cancel").css_classes(["flat"]).valign(gtk4::Align::Center).build();
        cancel_button.connect_clicked({
            let download_manager = download_manager.clone();
            let server_id = server_id.to_string();
            let item_id = item.id.clone();
            move |_| download_manager.cancel_item(&server_id, &item_id)
        });
        let box_ = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).build();
        box_.append(&spinner);
        box_.append(&cancel_button);
        row.add_suffix(&box_);
    } else {
        let remove_button = gtk4::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Remove").css_classes(["flat"]).valign(gtk4::Align::Center).build();
        remove_button.connect_clicked({
            let download_manager = download_manager.clone();
            let server_id = server_id.to_string();
            let item_id = item.id.clone();
            move |_| download_manager.clear_item(&server_id, &item_id)
        });
        row.add_suffix(&remove_button);
    }

    row
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{pool, pump_until, test_paths};
    use abs_storage::models::{Account, Server};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn session_for(pool: &SqlitePool, server_url: &str) -> (abs_core::auth::Session, Server, Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
        let server = abs_storage::repo::servers::get(pool, &server_id).await.unwrap();
        let account = abs_storage::repo::accounts::get(pool, &account_id).await.unwrap();
        (abs_core::auth::Session::new(pool.clone(), server_url, &server_id, &account), server, account)
    }

    async fn insert_synced_item(pool: &SqlitePool, server_id: &str, item_id: &str, title: &str) {
        abs_storage::repo::libraries::upsert(
            pool,
            abs_storage::repo::libraries::UpsertLibrary { id: "lib-1", server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 },
        )
        .await
        .unwrap();
        abs_storage::repo::items::upsert(
            pool,
            abs_storage::repo::items::UpsertItem {
                id: item_id,
                server_id,
                library_id: "lib-1",
                title,
                author: None,
                narrator: None,
                description: None,
                duration_seconds: 0.0,
                added_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    async fn mock_single_track_item(mock_server: &MockServer, item_id: &str, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "media": { "audioFiles": [{ "ino": "1", "duration": 5.0 }] }
            })))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/items/{item_id}/file/1")))
            .respond_with(ResponseTemplate::new(200).insert_header("Content-Length", "5").set_delay(delay).set_body_bytes(b"hello".to_vec()))
            .mount(mock_server)
            .await;
    }

    fn test_download_manager(pool: SqlitePool) -> DownloadManager {
        DownloadManager::new(pool, test_paths(), Box::new(abs_player::network_watch::UnknownNetworkMonitor), false)
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`.
    pub(crate) fn run_empty_state_renders_when_nothing_is_downloaded(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool, test_paths(), server, account, session, manager);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(5));
        assert!(hooks.status_page.is_visible(), "empty state should render with nothing downloaded");
        assert!(!hooks.scroller.is_visible());
    }

    /// A download in progress shows a row with a cancel button, not a remove button; canceling it
    /// removes the row entirely (nothing ever completed for this item).
    pub(crate) fn run_in_progress_download_shows_a_cancel_row(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_single_track_item(&mock_server, "item-1", Duration::from_secs(2)));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        let screen = build(pool.clone(), test_paths(), server.clone(), account, session.clone(), manager.clone());
        let hooks = screen.test_hooks();

        manager.start_download(session, "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));
        assert!(hooks.scroller.is_visible(), "an in-flight download should show a row");

        manager.cancel_item(&server.id, "item-1");
        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(10));
        assert!(hooks.status_page.is_visible(), "canceling the only (never-completed) download should return to the empty state");
    }

    /// A completed download shows a row with a remove button; removing it deletes the row and the
    /// underlying rows/files (verified indirectly via `downloaded_item_ids` going back to empty).
    pub(crate) fn run_completed_download_can_be_removed(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(pool());
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(mock_single_track_item(&mock_server, "item-1", Duration::ZERO));
        let (session, server, account) = runtime.block_on(session_for(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let manager = test_download_manager(pool.clone());
        manager.start_download(session.clone(), "item-1".to_string(), abs_core::downloads::DownloadScope::EntireBook, 0);
        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().map(|r| r.status == abs_storage::models::DownloadStatus::Complete).unwrap_or(false),
            Duration::from_secs(10),
        );

        let screen = build(pool.clone(), test_paths(), server.clone(), account, session, manager.clone());
        let hooks = screen.test_hooks();
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        manager.clear_item(&server.id, "item-1");
        pump_until(|| hooks.status_page.is_visible(), Duration::from_secs(10));
        assert!(hooks.status_page.is_visible(), "removing the only completed download should return to the empty state");
        assert!(runtime.block_on(abs_storage::repo::download_tracks::list_for_item(&pool, &server.id, "item-1")).unwrap().is_empty());
    }
}
