//! The Library tab's content — a searchable/sortable grid of every item across every synced
//! library, per `docs/design/ui-spec.md`'s "Library browse" section. Unlike Home's two curated
//! 10-item shelves, this shows everything.
//!
//! Deliberately out of scope for this pass (see the implementation plan): category chips (author/
//! series/genre — this app doesn't model series/genre at all yet), the view-options bottom sheet
//! ("Downloaded only", "Hide finished", "Grouping"), and sticky section headers when grouped (no
//! "group by" concept exists yet, and `GtkListBox` — used for list mode here, see
//! `abs_core::settings::LibraryViewMode` — has no native section-header support the way the
//! spec's literal `GtkListView` would). Search and sort are client-side over the already-synced
//! local table — neither `abs-storage` nor the real Audiobookshelf API surface this client uses
//! expose search/sort/pagination query params, so there is nothing server-side to delegate to yet.
//!
//! The grid/list choice is persisted via `abs_core::settings::{load,save}_library_view_mode` (a
//! plain key/value setting, same pattern as every other typed setting in that module) — restored
//! at startup and re-saved whenever the header's view toggle changes. Search text and sort order
//! are session-only, not persisted — the ui-spec doesn't ask for that, and a stale search filter
//! silently narrowing a freshly-opened Library screen would be a surprise, not a convenience.

use std::cell::Cell;
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

use abs_core::error::{CoreError, Result as CoreResult};
use abs_core::settings::LibraryViewMode;
use abs_storage::models::{Account, Item, Progress, Server};
use abs_storage::AppPaths;

use crate::player::PlayRequest;
use crate::widgets::item_card;

// Small enough that at least 2 columns fit at this app's default phone width (390px, see
// `application.rs`) once the flat button's own padding and the `GtkFlowBoxChild` wrapper's
// intrinsic padding are added on top of the raw cover size — confirmed live: 140 rendered as a
// single column with a lot of unused horizontal space, since the button+wrapper overhead alone was
// enough to push the cell's natural width just past half the available content width.
const TILE_SIZE: i32 = 108;

/// The header dropdown's indicator icons: the plain sort glyph, and the funnel that (Nautilus
/// style) signals "a filter is active" while the popover is closed.
const SORT_ICON: &str = "view-sort-descending-symbolic";
const FILTER_ACTIVE_ICON: &str = "funnel-symbolic";

#[derive(Clone)]
pub struct LibraryScreen {
    pub root: gtk4::Widget,
    /// Public so the main window's `win.open-library-search` keyboard action (ui-spec §6) can
    /// focus it — `TestHooks` is test-only by convention, and this is production wiring, not a
    /// test hook.
    pub search_entry: gtk4::SearchEntry,
    /// Kept on the screen so navigation-with-intent can land here pre-sorted/pre-filtered —
    /// Home's shelf headers call [`Self::apply_view`] through the shell. Same shared-handle
    /// posture as every other widget field: `LibraryWidgets` is cheap-clone, and the async
    /// pipeline already holds its own clone.
    widgets: LibraryWidgets,
    #[cfg(test)]
    hooks: TestHooks,
}

impl LibraryScreen {
    /// Navigation-with-intent entry point (docs/design/ui-spec.md, Home tap-through): Home's
    /// shelf headers switch to this screen pre-sorted — and, for Continue Listening,
    /// pre-filtered to in-progress books — without persisting anything, exactly like a manual
    /// sort/filter change (both are session-transient by the same reasoning: a view that
    /// silently hides books across restarts is a trap, not a convenience).
    pub(crate) fn apply_view(&self, key: SortKey, in_progress_only: bool) {
        self.widgets.sort.set(key);
        // `set_in_progress_only` re-renders with both the new sort and the new filter.
        set_in_progress_only(&self.widgets, in_progress_only);
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct TestHooks {
    pub status_page: adw::StatusPage,
    pub flow_box: gtk4::FlowBox,
    pub list_box: gtk4::ListBox,
    pub search_entry: gtk4::SearchEntry,
    pub banner: crate::widgets::banner::ErrorBanner,
    pub sort_buttons: SortButtons,
    pub sort_menu_button: gtk4::MenuButton,
    pub in_progress_check: gtk4::CheckButton,
    pub progress_banner: gtk4::Revealer,
    pub progress_show_all: gtk4::Button,
    pub view_toggle: gtk4::ToggleButton,
    pub offline_toggle: gtk4::ToggleButton,
    pub offline_banner: gtk4::Revealer,
    pub sync_now_button: gtk4::Button,
    pub toast_overlay: adw::ToastOverlay,
    pub scroller: gtk4::ScrolledWindow,
}

#[cfg(test)]
impl LibraryScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SortKey {
    DateAdded,
    Title,
    Author,
    Duration,
    /// Not a metadata field but a playback one: items with a progress row first, newest
    /// last-listen first (`progress.updated_at` — the server's own last-update time on import),
    /// never-played items after them in stable order. Backs Home's "Continue Listening"
    /// tap-through and the popover's "Last listened" entry.
    LastListened,
}

#[derive(Clone)]
pub struct SortButtons {
    pub date_added: gtk4::Button,
    pub title: gtk4::Button,
    pub author: gtk4::Button,
    pub duration: gtk4::Button,
    pub last_listened: gtk4::Button,
}

/// Everything `render` needs a handle to, cloned as a whole into the `spawn_future_local` block —
/// every field is a reference-counted GTK/Adwaita widget handle, so cloning is cheap and shares
/// the same underlying widgets, not copies. Mirrors `home.rs`'s `HomeWidgets` shape.
#[derive(Clone)]
struct LibraryWidgets {
    status_page: adw::StatusPage,
    scroller: gtk4::ScrolledWindow,
    flow_box: gtk4::FlowBox,
    list_box: gtk4::ListBox,
    banner: crate::widgets::banner::ErrorBanner,
    search_entry: gtk4::SearchEntry,
    sort: Rc<Cell<SortKey>>,
    /// The one knob behind every "in progress only" surface — the popover's CheckButton, the
    /// header button's funnel indicator, and the in-view banner all read/write it through
    /// [`set_in_progress_only`] so they can't drift apart. Session-transient like `sort`.
    in_progress_only: Rc<Cell<bool>>,
    view_mode: Rc<Cell<LibraryViewMode>>,
    /// Shared state per `docs/design/ui-spec.md` ("not a per-screen setting") — this screen and
    /// Home each load/save the same `abs_core::settings::{load,save}_offline_mode` key, same
    /// reasoning `LibraryViewMode` already established for a different persisted toggle.
    offline_mode: Rc<Cell<bool>>,
    data: Rc<std::cell::RefCell<LibraryData>>,
    on_play: Rc<dyn Fn(PlayRequest)>,
    /// Header dropdown button — mutated only to reflect the filter state (funnel icon while a
    /// filter is active, Nautilus-style), never to *own* it.
    sort_menu_button: gtk4::MenuButton,
    in_progress_check: gtk4::CheckButton,
    progress_banner: gtk4::Revealer,
}

struct LibraryData {
    items: Vec<Item>,
    /// Ids of items downloaded fully or partially — loaded once alongside `items` (one query, not
    /// one per card). Feeds `item_card::build`'s read-only download badge.
    downloaded: std::collections::HashSet<String>,
    /// Every progress row for the account, keyed by item id — backs the `LastListened` sort and
    /// the "In progress only" filter. Loaded in the same one-query-per-concern pass as
    /// `downloaded`, never per card.
    last_listened: std::collections::HashMap<String, Progress>,
}

/// Builds the screen. Signature mirrors `home::build`'s exactly — same reasoning: `server`/
/// `account` are already-resolved rows the caller looks up once, and `on_play` is how tapping a
/// cover starts playback without this screen ever touching `abs-player`/`abs-core::streaming`
/// itself. `on_relogin` routes the banner's "Log in again" action (authorization failures only)
/// back to the shell, same as Home's.
pub fn build(
    pool: SqlitePool,
    paths: AppPaths,
    server: Server,
    account: Account,
    session: abs_core::auth::Session,
    on_play: impl Fn(PlayRequest) + Clone + 'static,
    on_relogin: impl Fn() + Clone + 'static,
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
        last_listened: gtk4::Button::builder().label("Last listened").build(),
    };
    // HIG popovers: group same-type controls under section headings — this popover holds two
    // kinds of view option (sort entries vs. filter toggles), so each gets its own caption
    // rather than one ambiguous list.
    let popover_section_label = |text: &str| {
        gtk4::Label::builder().label(text).xalign(0.0).css_classes(["caption", "dim-label"]).margin_top(6).margin_bottom(2).margin_start(6).margin_end(6).build()
    };
    let sort_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(2).build();
    sort_box.append(&popover_section_label("Sort"));
    sort_box.append(&sort_buttons.date_added);
    sort_box.append(&sort_buttons.title);
    sort_box.append(&sort_buttons.author);
    sort_box.append(&sort_buttons.duration);
    sort_box.append(&sort_buttons.last_listened);

    // The one manual home of the "In progress only" filter — the same state Home's Continue
    // Listening header sets on tap-through (see `apply_view`). All state surfaces (this check,
    // the banner below, the header icon) funnel through `set_in_progress_only`.
    let in_progress_check = gtk4::CheckButton::builder().label("In progress only").margin_top(4).margin_bottom(4).margin_start(6).margin_end(6).build();
    sort_box.append(&popover_section_label("Filter"));
    sort_box.append(&in_progress_check);

    let sort_popover = gtk4::Popover::builder().child(&sort_box).build();
    let sort_menu_button = gtk4::MenuButton::builder().icon_name(SORT_ICON).tooltip_text("Sort & filter").popover(&sort_popover).build();
    header.pack_end(&sort_menu_button);

    // The filter's ambient indicator while the popover is closed (the funnel icon on the header
    // button is only a hint) — an in-view "why are books hidden" banner with a one-tap escape,
    // built from the same banner row as the offline banner below. `ErrorBanner` is wrong here:
    // this is a neutral filter notice, not a failure.
    // Shared by both filter notices (this one and the offline banner below) so the two can't
    // drift apart — the caption label gets its own lightly-margined row rather than hugging the
    // screen edge. This banner alone appends its "Show all" escape button to the returned row.
    let caption_banner_row = |text: &str| {
        let label = gtk4::Label::builder().label(text).xalign(0.0).hexpand(true).css_classes(["caption", "dim-label"]).build();
        let row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(8).margin_start(12).margin_end(12).margin_top(4).margin_bottom(4).build();
        row.append(&label);
        (label, row)
    };
    let (_, progress_banner_row) = caption_banner_row("Showing books in progress");
    let progress_show_all = gtk4::Button::builder().label("Show all").css_classes(["flat"]).valign(gtk4::Align::Center).build();
    progress_banner_row.append(&progress_show_all);
    let progress_banner = gtk4::Revealer::builder().transition_type(gtk4::RevealerTransitionType::SlideDown).child(&progress_banner_row).reveal_child(false).build();

    // The spec's real toggle lives inside a not-yet-built "view options" bottom sheet (see this
    // module's doc comment) — matching how sort was already implemented as a plain popover instead
    // of that full sheet, this is a single toggle button rather than the sheet. Starts showing the
    // "switch to list" icon since Grid is the default mode.
    let view_toggle = gtk4::ToggleButton::builder().icon_name("view-list-symbolic").tooltip_text("List view").build();
    header.pack_end(&view_toggle);

    // Offline-mode toggle (ui-spec: "leading side, opposite the avatar" on Home; mirrored here on
    // the leading side too, alongside the search entry). Shared persisted state with Home's own
    // toggle, not a per-screen setting — see `offline_mode`'s field doc.
    let offline_toggle = gtk4::ToggleButton::builder().icon_name("airplane-mode-symbolic").tooltip_text("Offline mode").build();
    header.pack_start(&offline_toggle);

    // "Sync now" (ui-spec Library browse) — the same header-bar ⋯ overflow as Home's, forcing
    // an immediate resync; the pull-to-refresh gesture on the main scroller (wired below) runs
    // the exact same manual path. Plain `GtkMenuButton` + flat button, per this crate's
    // no-GMenu convention.
    let sync_now_button = gtk4::Button::builder().label("Sync now").css_classes(["flat"]).build();
    let sync_menu_popover = gtk4::Popover::builder().child(&sync_now_button).build();
    let sync_menu_button = gtk4::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More")
        .popover(&sync_menu_popover)
        .build();
    header.pack_end(&sync_menu_button);

    let (_, offline_banner_row) = caption_banner_row("Showing downloaded items only");
    let offline_banner = gtk4::Revealer::builder().transition_type(gtk4::RevealerTransitionType::SlideDown).child(&offline_banner_row).reveal_child(false).build();

    let banner = crate::widgets::banner::ErrorBanner::new();
    // The "Log in again" action (revealed only on authorization failures) routes to the shell,
    // same as Home's.
    banner.action_button().connect_clicked({
        let on_relogin = on_relogin.clone();
        move |_| on_relogin()
    });

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

    // List mode: a plain `GtkListBox` of `AdwActionRow`s (same "boxed list" pattern `home.rs`'s
    // `libraries_list` already uses), not the spec's literal `GtkListView` — this codebase already
    // substitutes a simpler widget for the grid too (`GtkFlowBox`, not `GtkGridView`). Hidden until
    // the user switches to List mode.
    let list_box = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(16)
        .margin_end(16)
        .margin_top(12)
        .margin_bottom(16)
        .visible(false)
        .build();

    let scroll_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    scroll_content.append(&flow_box);
    scroll_content.append(&list_box);

    let scroller = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).child(&scroll_content).build();

    let status_page = adw::StatusPage::builder()
        .icon_name("folder-music-symbolic")
        .title("No items yet")
        .description("Check your connection and try again.")
        .vexpand(true)
        .visible(false)
        .build();

    // The banners live directly under the header bar, outside the scroller — same reason as
    // home.rs's identically-placed banners: `apply` hides the scroller whenever nothing is
    // cached (zero items renders the status page instead), and a banner trapped inside the
    // hidden scroller disappears without a trace. Above the status page too, so the failure
    // is visible in every state.
    let body = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).vexpand(true).build();
    body.append(&offline_banner);
    body.append(&progress_banner);
    body.append(banner.widget());
    body.append(&scroller);
    body.append(&status_page);

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&body);

    // Toasts float over the whole screen, header bar included — same shape as the player
    // screen's and Home's own overlays. Manual sync triggers report through this one.
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&root));

    let widgets = LibraryWidgets {
        status_page: status_page.clone(),
        scroller: scroller.clone(),
        flow_box: flow_box.clone(),
        list_box: list_box.clone(),
        banner: banner.clone(),
        search_entry: search_entry.clone(),
        sort: Rc::new(Cell::new(SortKey::DateAdded)),
        in_progress_only: Rc::new(Cell::new(false)),
        view_mode: Rc::new(Cell::new(LibraryViewMode::Grid)),
        offline_mode: Rc::new(Cell::new(false)),
        data: Rc::new(std::cell::RefCell::new(LibraryData { items: Vec::new(), downloaded: std::collections::HashSet::new(), last_listened: std::collections::HashMap::new() })),
        on_play: Rc::new(on_play),
        sort_menu_button: sort_menu_button.clone(),
        in_progress_check: in_progress_check.clone(),
        progress_banner: progress_banner.clone(),
    };

    // The filter's two manual entry points: the popover's check and the banner's "Show all".
    // Both funnel through `set_in_progress_only`; no signal-blocking is needed when it writes
    // the check back — `set_active` to the value it already holds doesn't re-emit `toggled`,
    // so the write-back terminates immediately (and re-syncs rather than fights).
    in_progress_check.connect_toggled({
        let widgets = widgets.clone();
        move |check| set_in_progress_only(&widgets, check.is_active())
    });
    progress_show_all.connect_clicked({
        let widgets = widgets.clone();
        move |_| set_in_progress_only(&widgets, false)
    });

    search_entry.connect_search_changed({
        let widgets = widgets.clone();
        move |_| render_from_current_data(&widgets)
    });

    let view_toggle_handler = view_toggle.connect_toggled({
        let pool = pool.clone();
        let widgets = widgets.clone();
        move |toggle| {
            let mode = if toggle.is_active() { LibraryViewMode::List } else { LibraryViewMode::Grid };
            apply_view_mode(mode, &widgets, toggle);
            glib::spawn_future_local({
                let pool = pool.clone();
                async move {
                    if let Err(err) = abs_core::settings::save_library_view_mode(&pool, mode).await {
                        tracing::warn!(%err, "couldn't persist the Library view mode; it won't be remembered next launch");
                    }
                }
            });
        }
    });

    // Loaded once, asynchronously, right after construction (a local-only DB read, no network).
    // The toggle's own handler is blocked while this sets the *initial* `active` state — without
    // that, restoring a persisted List mode would itself fire `connect_toggled`, which would
    // immediately re-save the exact value it just loaded (harmless, but a pointless write on every
    // single launch) and — worse, if a future edit made that handler do anything not idempotent —
    // a real bug waiting to happen.
    glib::spawn_future_local({
        let pool = pool.clone();
        let widgets = widgets.clone();
        let view_toggle = view_toggle.clone();
        async move {
            if let Ok(mode) = abs_core::settings::load_library_view_mode(&pool).await {
                view_toggle.block_signal(&view_toggle_handler);
                view_toggle.set_active(mode == LibraryViewMode::List);
                view_toggle.unblock_signal(&view_toggle_handler);
                apply_view_mode(mode, &widgets, &view_toggle);
            }
        }
    });

    let offline_toggle_server_id = server.id.clone();
    let offline_toggle_handler = offline_toggle.connect_toggled({
        let pool = pool.clone();
        let widgets = widgets.clone();
        let offline_banner = offline_banner.clone();
        let server_id = offline_toggle_server_id.clone();
        move |toggle| {
            let active = toggle.is_active();
            widgets.offline_mode.set(active);
            offline_banner.set_reveal_child(active);
            // Rendered synchronously, before any DB work: the visible filter change must not be
            // gated behind the refetch's pool acquire — on a contended pool (sync/cover/progress
            // cycles all fighting over the 5 connections) that await has been observed stalling
            // for tens of seconds, leaving the grid unfiltered the whole time. The in-memory
            // `downloaded` set is whatever the last full load saw, which is immediate-and-slightly-
            // stale; the spawned refetch below re-renders with fresh data when it lands.
            render_from_current_data(&widgets);
            glib::spawn_future_local({
                let pool = pool.clone();
                let widgets = widgets.clone();
                let server_id = server_id.clone();
                async move {
                    // Refetched here rather than trusting whatever `data.downloaded` last held
                    // from the sync pipeline — a download can complete in the background (the
                    // Player screen's download button) well after Library's last full load, and
                    // toggling offline mode should reflect the *current* download state, not a
                    // stale snapshot.
                    if let Ok(downloaded) = abs_core::download_tracks::downloaded_item_ids(&pool, &server_id).await {
                        widgets.data.borrow_mut().downloaded = downloaded.into_iter().collect();
                    }
                    render_from_current_data(&widgets);

                    if let Err(err) = abs_core::settings::save_offline_mode(&pool, active).await {
                        tracing::warn!(%err, "couldn't persist offline mode; it won't be remembered next launch");
                    }
                    if let Ok(mut options) = abs_core::settings::load_library_view_options(&pool).await {
                        options.downloaded_only = active;
                        if let Err(err) = abs_core::settings::save_library_view_options(&pool, &options).await {
                            tracing::warn!(%err, "couldn't persist the Library downloaded-only view option");
                        }
                    }
                }
            });
        }
    });

    // Same "load once, blocking the handler while restoring" pattern as the view-mode toggle
    // above — the persisted value is shared with Home's own toggle (ui-spec: "not a per-screen
    // setting"), so this screen must reflect whatever was last set from either tab.
    glib::spawn_future_local({
        let pool = pool.clone();
        let widgets = widgets.clone();
        let offline_toggle = offline_toggle.clone();
        let offline_banner = offline_banner.clone();
        let server_id = offline_toggle_server_id;
        async move {
            if let Ok(active) = abs_core::settings::load_offline_mode(&pool).await {
                offline_toggle.block_signal(&offline_toggle_handler);
                offline_toggle.set_active(active);
                offline_toggle.unblock_signal(&offline_toggle_handler);
                widgets.offline_mode.set(active);
                offline_banner.set_reveal_child(active);
                if active {
                    if let Ok(downloaded) = abs_core::download_tracks::downloaded_item_ids(&pool, &server_id).await {
                        widgets.data.borrow_mut().downloaded = downloaded.into_iter().collect();
                    }
                }
                render_from_current_data(&widgets);
            }
        }
    });

    for (button, key) in [
        (&sort_buttons.date_added, SortKey::DateAdded),
        (&sort_buttons.title, SortKey::Title),
        (&sort_buttons.author, SortKey::Author),
        (&sort_buttons.duration, SortKey::Duration),
        (&sort_buttons.last_listened, SortKey::LastListened),
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

    let ctx = SyncCtx {
        pool: pool.clone(),
        paths: paths.clone(),
        session: session.clone(),
        server_id: server.id.clone(),
        account_id: account.id.clone(),
    };
    spawn_sync_cycle(ctx.clone(), widgets.clone(), None);

    // The two manual triggers — the ⋯ menu's "Sync now" and a pull past the scroller's top —
    // share one `ManualSync` (toast + in-flight guard) between them, mirroring home.rs.
    let manual_sync = crate::widgets::ManualSync::new(&toast_overlay);
    {
        let ctx = ctx.clone();
        let widgets = widgets.clone();
        let manual_sync = manual_sync.clone();
        let popover = sync_menu_popover.clone();
        sync_now_button.connect_clicked(move |_| {
            popover.popdown();
            spawn_sync_cycle(ctx.clone(), widgets.clone(), Some(manual_sync.clone()));
        });
    }
    {
        let ctx = ctx.clone();
        let widgets = widgets.clone();
        let manual_sync = manual_sync.clone();
        crate::widgets::pull_to_refresh::attach(&scroller, move || {
            spawn_sync_cycle(ctx.clone(), widgets.clone(), Some(manual_sync.clone()));
        });
    }

    LibraryScreen {
        root: toast_overlay.clone().upcast(),
        search_entry: search_entry.clone(),
        widgets: widgets.clone(),
        #[cfg(test)]
        hooks: TestHooks {
            status_page,
            flow_box,
            list_box,
            search_entry,
            banner,
            sort_buttons,
            sort_menu_button,
            in_progress_check,
            progress_banner,
            progress_show_all,
            view_toggle,
            offline_toggle,
            offline_banner,
            sync_now_button,
            toast_overlay,
            scroller,
        },
    }
}

/// What a sync cycle needs, bundled for re-spawning — the manual triggers (the ⋯ menu's
/// "Sync now", pull-to-refresh) re-run the exact cycle the screen opens with. Mirrors home.rs's
/// `SyncCtx`; `server`/`account` ride along as the ids the pipeline actually uses.
#[derive(Clone)]
struct SyncCtx {
    pool: SqlitePool,
    paths: AppPaths,
    session: abs_core::auth::Session,
    server_id: String,
    account_id: String,
}

/// Runs one full sync cycle on the main loop — the same 3-phase render shape as `home.rs`:
/// render from cache immediately, sync + reconcile progress, re-render, then fetch cover art
/// concurrently for everything just rendered and re-render once more. This is the only place
/// this screen touches `abs_core`; it never imports `abs_api` at all. Called once when the
/// screen is built and again on every manual trigger, so it must leave all widget state
/// consistent no matter how many times it runs.
///
/// As in home.rs, the network/DB pipeline runs inside `tokio::spawn` (worker threads) — the
/// `spawn_future_local` future is polled on the GTK main thread, and doing HTTP body handling,
/// statement building and cache writes directly there steals frames from the main loop. The
/// main context only parks on the `JoinHandle`s and applies widget updates between stages.
///
/// A manual trigger (`manual`) reports its outcome as a toast and shares an in-flight guard
/// with the screen's other manual trigger — see `crate::widgets::ManualSync`. The automatic
/// cycle passes none and stays unguarded.
fn spawn_sync_cycle(ctx: SyncCtx, widgets: LibraryWidgets, manual: Option<crate::widgets::ManualSync>) {
    if let Some(manual) = &manual {
        if !manual.claim() {
            return;
        }
    }
    glib::spawn_future_local(async move {
        let SyncCtx { pool, paths, session, server_id, account_id } = ctx;

        if let Ok(data) = load(&pool, &server_id, &account_id).await {
            apply(data, &widgets);
        }

        let spawned_sync = tokio::spawn({
            let pool = pool.clone();
            let session = session.clone();
            let server_id = server_id.clone();
            let account_id = account_id.clone();
            async move {
                // Asked at call time, not captured at build time — see home.rs's pipeline note.
                // The connection (settings + resolved base URL) is asked the same way, so a
                // settings change is honored by the next sync cycle.
                let access_token = session.access_token().await;
                let connection = match session.connection_target().await {
                    Ok(connection) => connection,
                    Err(err) => {
                        tracing::warn!(%err, "couldn't load the server's connection settings; sync skipped");
                        return Err(err);
                    }
                };
                let sync_result = abs_core::sync::sync_all(&pool, &connection, &server_id, &access_token).await;

                if let Err(err) = abs_core::progress_sync::reconcile_all_progress(&pool, &connection, &access_token, &account_id, &server_id).await
                {
                    tracing::warn!(%err, "couldn't reconcile progress with the server; showing local progress");
                }

                sync_result
            }
        });
        let sync_result = spawned_sync.await.expect("the Library sync task must not panic");
        let manual_ok = sync_result.is_ok();

        let data_after_sync = load(&pool, &server_id, &account_id).await.ok();
        let item_ids_after_sync: Vec<String> = data_after_sync.as_ref().map(|data| data.items.iter().map(|item| item.id.clone()).collect()).unwrap_or_default();
        if let Some(data) = data_after_sync {
            apply(data, &widgets);
        }

        match &sync_result {
            Ok(()) => widgets.banner.set_revealed(false),
            Err(err) => {
                // An authorization failure is not fixable by re-syncing — the session itself
                // is what died — so the banner swaps its copy and grows a "Log in again"
                // action routed to the shell, mirroring Home's failure state.
                if matches!(err, CoreError::Auth) {
                    widgets.banner.set_title("Session expired — showing what's cached.");
                    widgets.banner.set_action_label(Some("Log in again"));
                } else {
                    widgets.banner.set_title("Couldn't sync — showing what's cached.");
                    widgets.banner.set_action_label(None);
                }
                widgets.banner.set_details(Some(&err.to_string()));
                widgets.banner.set_revealed(true);
            }
        }

        // Cover art is cosmetic and best-effort (same posture as Home's own cover fetch),
        // fetched concurrently for everything just rendered — after the rest of the screen
        // is already showing (the `apply` above), so a slow/offline server delays only the
        // artwork, never the initial post-sync render. Same two-stage `tokio::spawn` shape
        // as home.rs's identically-named cycle; the batch shares one HTTP client (one pooled
        // connection) across all of it.
        if !item_ids_after_sync.is_empty() {
            let spawned_covers = tokio::spawn({
                let pool = pool.clone();
                let paths = paths.clone();
                let session = session.clone();
                let server_id = server_id.clone();
                async move {
                    let access_token = session.access_token().await;
                    // Best-effort like the fetches themselves: a settings failure here just
                    // means no covers — logged (above), never surfaced.
                    if let Some(connection) = session.connection_target().await.ok().as_ref() {
                        abs_core::covers::fetch_and_cache_covers(&paths, &pool, connection, &access_token, &server_id, item_ids_after_sync).await;
                    }
                }
            });
            spawned_covers.await.expect("the Library cover-fetch task must not panic");

            if let Ok(data) = load(&pool, &server_id, &account_id).await {
                apply(data, &widgets);
            }
        }

        // The manual trigger's own feedback — after the resolve above, so the banner already
        // shows whatever the toast is about; "Sync failed" carries no details itself.
        if let Some(manual) = manual {
            manual.finish(manual_ok);
        }
    });
}

/// The single writer behind every "in progress only" surface — the popover's check, the header
/// button's funnel indicator + tooltip, and the in-view banner all change here, from the one
/// `Cell`, so the four can't drift apart — and the visible list re-renders, since this is the
/// only place the `Cell` changes. Writing the check back is loop-safe: `set_active` to the
/// value it already holds doesn't re-emit `toggled` (and the toggled handler routes back here,
/// where the second write is a no-op).
fn set_in_progress_only(widgets: &LibraryWidgets, active: bool) {
    widgets.in_progress_only.set(active);
    widgets.progress_banner.set_reveal_child(active);
    widgets.sort_menu_button.set_icon_name(if active { FILTER_ACTIVE_ICON } else { SORT_ICON });
    widgets.sort_menu_button.set_tooltip_text(Some(if active { "Filter active — sort & filter" } else { "Sort & filter" }));
    if widgets.in_progress_check.is_active() != active {
        widgets.in_progress_check.set_active(active);
    }
    render_from_current_data(widgets);
}

/// Reads whatever's currently cached locally across every synced library — never talks to the
/// network. Unlike Home's `load()`, nothing is truncated or pre-sorted here: filtering/sorting for
/// display happens in `render_visible` against the search text, the filter and the chosen
/// `SortKey`. The account's progress rows ride along in the same pass — they back the
/// `LastListened` sort and the "In progress only" filter.
async fn load(pool: &SqlitePool, server_id: &str, account_id: &str) -> CoreResult<LibraryData> {
    let libraries = abs_storage::repo::libraries::list_for_server(pool, server_id).await?;
    let mut items = Vec::new();
    for library in &libraries {
        items.extend(abs_storage::repo::items::list_for_library(pool, server_id, &library.id).await?);
    }
    let downloaded = abs_core::download_tracks::downloaded_item_ids(pool, server_id).await?.into_iter().collect();
    let last_listened = abs_storage::repo::progress::list_for_account(pool, account_id).await?.into_iter().map(|progress| (progress.item_id.clone(), progress)).collect();
    Ok(LibraryData { items, downloaded, last_listened })
}

fn apply(data: LibraryData, widgets: &LibraryWidgets) {
    let has_any_item = !data.items.is_empty();
    *widgets.data.borrow_mut() = data;
    widgets.status_page.set_visible(!has_any_item);
    widgets.scroller.set_visible(has_any_item);
    render_from_current_data(widgets);
}

/// Applies a view mode to every widget it affects — the toggle button's own icon/tooltip, which
/// container is visible, and a re-render — and updates `widgets.view_mode` first so that
/// re-render sees the new mode. Shared by the toggle's `connect_toggled` handler and by restoring
/// the persisted mode at startup, so both paths stay in sync by construction rather than by
/// keeping two copies of this logic in step by hand.
fn apply_view_mode(mode: LibraryViewMode, widgets: &LibraryWidgets, toggle: &gtk4::ToggleButton) {
    widgets.view_mode.set(mode);
    toggle.set_icon_name(if mode == LibraryViewMode::List { "view-grid-symbolic" } else { "view-list-symbolic" });
    toggle.set_tooltip_text(Some(if mode == LibraryViewMode::List { "Grid view" } else { "List view" }));
    widgets.flow_box.set_visible(mode == LibraryViewMode::Grid);
    widgets.list_box.set_visible(mode == LibraryViewMode::List);
    render_from_current_data(widgets);
}

fn render_from_current_data(widgets: &LibraryWidgets) {
    let query = widgets.search_entry.text().to_lowercase();
    let sort = widgets.sort.get();
    let data = widgets.data.borrow();

    let offline_mode = widgets.offline_mode.get();
    let mut visible: Vec<&Item> = data
        .items
        .iter()
        .filter(|item| {
            query.is_empty()
                || item.title.to_lowercase().contains(&query)
                || item.author.as_deref().is_some_and(|author| author.to_lowercase().contains(&query))
        })
        .filter(|item| !offline_mode || data.downloaded.contains(&item.id))
        // "In progress only" (Home's Continue Listening tap-through, or the popover's check):
        // needs a progress row that exists *and* isn't finished — a completed book has both,
        // a never-played one has neither.
        .filter(|item| {
            !widgets.in_progress_only.get()
                || data.last_listened.get(&item.id).is_some_and(|progress| !progress.is_finished)
        })
        .collect();

    match sort {
        SortKey::DateAdded => visible.sort_by_key(|item| std::cmp::Reverse(item.added_at)),
        SortKey::Title => visible.sort_by_key(|a| a.title.to_lowercase()),
        SortKey::Author => visible.sort_by(|a, b| {
            a.author.as_deref().unwrap_or("").to_lowercase().cmp(&b.author.as_deref().unwrap_or("").to_lowercase())
        }),
        SortKey::Duration => visible.sort_by(|a, b| b.duration_seconds.total_cmp(&a.duration_seconds)),
        // Played before never-played; within played, newest last-listen first. `updated_at`
        // carries the *true* last-listened time (the server's own `lastUpdate` on import), so
        // this reads as "what was I listening to lately", not "what got imported lately".
        SortKey::LastListened => visible.sort_by(|a, b| match (data.last_listened.get(&a.id), data.last_listened.get(&b.id)) {
            (Some(a_progress), Some(b_progress)) => b_progress.updated_at.cmp(&a_progress.updated_at),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }),
    }

    let has_visible = !visible.is_empty();

    // Only the active container is rebuilt — same "full rebuild on every render, not incremental"
    // posture already used everywhere else in this file, just gated per mode so switching modes
    // (or searching/sorting while a mode is hidden) doesn't do wasted work on the other one.
    match widgets.view_mode.get() {
        LibraryViewMode::Grid => {
            clear_flow_box(&widgets.flow_box);
            for item in visible {
                let subtitle = item_subtitle(item);
                widgets.flow_box.insert(&item_card::build(TILE_SIZE, item, &subtitle, &widgets.on_play, true, data.downloaded.contains(&item.id)), -1);
            }
        }
        LibraryViewMode::List => {
            clear_list_box(&widgets.list_box);
            for item in visible {
                widgets.list_box.append(&library_list_row(item, &widgets.on_play));
            }
        }
    }

    // Offline mode overrides the empty state with "No downloaded items" (ui-spec LB-7) rather than
    // whatever `apply()` last set from the unfiltered sync result — search/sort's own empty case
    // is left as-is (a pre-existing gap outside this pass's scope), only offline mode's filter
    // gets this treatment since it's the one new way this screen can legitimately show nothing.
    if offline_mode {
        widgets.status_page.set_title(if has_visible { "No items yet" } else { "No downloaded items" });
        widgets.status_page.set_visible(!has_visible);
        widgets.scroller.set_visible(has_visible);
    } else if !data.items.is_empty() {
        widgets.status_page.set_title("No items yet");
    }
}

fn item_subtitle(item: &Item) -> String {
    let hours = item.duration_seconds / 3600.0;
    format!("{} · {hours:.1}h", item.author.as_deref().unwrap_or("Unknown author"))
}

/// A list-mode row — same information as a grid tile (cover thumbnail, title, subtitle), just laid
/// out horizontally per `docs/design/ui-spec.md`'s "useful for podcast episode-style feeds" framing.
/// Mirrors `home.rs`'s `library_row()` shape (an `AdwActionRow` with a prefix), swapping the
/// symbolic icon for a small cover thumbnail via the same `CoverImage` widget `item_card.rs` uses.
fn library_list_row(item: &Item, on_play: &Rc<dyn Fn(PlayRequest)>) -> adw::ActionRow {
    const THUMBNAIL_SIZE: i32 = 48;

    let cover = crate::widgets::cover_image::CoverImage::new(THUMBNAIL_SIZE);
    cover.set_path(item.cover_cache_path.as_deref().map(std::path::Path::new));

    let row = adw::ActionRow::builder().title(&item.title).subtitle(item_subtitle(item)).activatable(true).build();
    row.add_prefix(cover.widget());

    let request = PlayRequest { item_id: item.id.clone(), title: item.title.clone(), author: item.author.clone() };
    let on_play = on_play.clone();
    row.connect_activated(move |_| on_play(request.clone()));

    row
}

fn clear_flow_box(fb: &gtk4::FlowBox) {
    while let Some(child) = fb.child_at_index(0) {
        fb.remove(&child);
    }
}

fn clear_list_box(lb: &gtk4::ListBox) {
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

    /// Seeds the library/items/progress rows a scenario's progress-dependent assertions read,
    /// before `build`'s first cached load — the progress table's foreign key needs the items to
    /// exist first. Every write is an idempotent upsert (the sync re-runs the same ones against
    /// the mock server's identical ids), and the library id matches the mock's.
    async fn seed_item_with_progress(pool: &SqlitePool, server_id: &str, account_id: &str, item_id: &str, title: &str, added_at_ms: i64, progress: Option<(f64, bool, chrono::DateTime<chrono::Utc>)>) {
        abs_storage::repo::libraries::upsert(
            pool,
            abs_storage::repo::libraries::UpsertLibrary { id: "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", server_id, name: "Audiobooks", media_type: "book", icon: None, display_order: 1 },
        )
        .await
        .unwrap();
        abs_storage::repo::items::upsert(
            pool,
            abs_storage::repo::items::UpsertItem { id: item_id, server_id, library_id: "e4bb1afb-4a4f-4dd6-8be0-e615d233185b", title, author: None, narrator: None, description: None, duration_seconds: 3600.0, added_at: chrono::DateTime::from_timestamp_millis(added_at_ms).unwrap() },
        )
        .await
        .unwrap();
        if let Some((position, is_finished, updated_at)) = progress {
            abs_storage::repo::progress::set_at(pool, account_id, server_id, item_id, position, is_finished, updated_at).await.unwrap();
        }
    }

    async fn account_and_server(pool: &SqlitePool, server_url: &str) -> (Server, Account) {
        let server_id = abs_storage::repo::servers::add(pool, server_url).await.unwrap();
        let account_id = abs_storage::repo::accounts::add(pool, &server_id, "jane", "token123", None).await.unwrap();
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

    fn list_box_titles(list_box: &gtk4::ListBox) -> Vec<String> {
        let mut titles = Vec::new();
        let mut index = 0;
        while let Some(row) = list_box.row_at_index(index) {
            let action_row = row.downcast::<adw::ActionRow>().expect("list box row is an AdwActionRow");
            titles.push(action_row.title().to_string());
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));

        hooks.search_entry.set_text("weir");
        pump_until(|| flow_box_titles(&hooks.flow_box).len() == 1, Duration::from_secs(5));

        let titles = flow_box_titles(&hooks.flow_box);
        assert_eq!(titles, vec!["Project Hail Mary"], "searching an author substring should filter to matching items");

        hooks.search_entry.set_text("dune");
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Dune".to_string()], Duration::from_secs(5));
    }

    /// Toggling offline mode should narrow the grid to items with at least one completed
    /// download, show the banner, and restore the full list when toggled off again.
    pub(crate) fn run_offline_mode_toggle_filters_to_downloaded_items(runtime: &tokio::runtime::Runtime) {
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool.clone(), crate::test_support::test_paths(), server.clone(), account, session, |_| {}, || {});
        let hooks = screen.test_hooks();
        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));

        // Mark "item-1" as downloaded (one complete track) directly in storage — this test is
        // about the toggle/filter, not the download pipeline itself.
        runtime.block_on(abs_storage::repo::tracks::upsert_all(&pool, &server.id, "item-1", &[abs_storage::repo::tracks::NewTrack { ino: "1", duration_seconds: 3600.0, offset_seconds: 0.0 }])).unwrap();
        runtime.block_on(abs_storage::repo::download_tracks::upsert_pending(&pool, &server.id, "item-1", "1", "/p/1.mp3")).unwrap();
        runtime.block_on(abs_storage::repo::download_tracks::mark_complete(&pool, &server.id, "item-1", "1", 10)).unwrap();

        // The filter must land synchronously on the toggle, not after the handler's background
        // refetch resolves — that refetch's pool acquire has been observed stalling for tens of
        // seconds on a contended device pool, leaving the grid unfiltered the whole time (and
        // only a view-mode round-trip forced an immediate re-render). The first toggle-on still
        // waits for the refetch here, because the download above was inserted after this
        // screen's load — the in-memory set is stale and the synchronous render legitimately
        // shows nothing — but every later toggle below round-trips with no pump at all: the
        // list re-renders inside the handler, from the by-then-warm in-memory set.
        hooks.offline_toggle.set_active(true);
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Project Hail Mary".to_string()], Duration::from_secs(5));
        assert!(hooks.offline_banner.reveals_child(), "the offline banner should show while the toggle is active");

        hooks.offline_toggle.set_active(false);
        assert_eq!(flow_box_titles(&hooks.flow_box).len(), 2, "toggling offline mode off must re-render synchronously, not after the handler's DB refetch");
        assert!(!hooks.offline_banner.reveals_child(), "the banner should hide once offline mode is off");

        hooks.offline_toggle.set_active(true);
        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Project Hail Mary".to_string()], "toggling offline mode on must filter synchronously from the in-memory downloaded set");
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));

        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Zed Book", "Alpha Book"], "default sort is date-added descending");

        hooks.sort_buttons.title.emit_clicked();
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Alpha Book".to_string(), "Zed Book".to_string()], Duration::from_secs(5));

        hooks.sort_buttons.date_added.emit_clicked();
        pump_until(|| flow_box_titles(&hooks.flow_box) == vec!["Zed Book".to_string(), "Alpha Book".to_string()], Duration::from_secs(5));
    }

    /// "Last listened" orders by actual listening recency — played items newest-listen first,
    /// never-played items after them in stable order — independent of when items were *added*
    /// to the server.
    pub(crate) fn run_sorts_by_last_listened(runtime: &tokio::runtime::Runtime) {
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
                        item_json("item-2", "Alpha Book", "Author B", 1_600_000_000_000, 3600.0),
                        item_json("item-3", "Middle Book", "Author C", 1_500_000_000_000, 3600.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        let now = chrono::Utc::now();
        // Added newest-first (the default sort's order), but listened to in the opposite order —
        // plus one item never played at all.
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-1", "Zed Book", 1_700_000_000_000, Some((600.0, false, now - chrono::Duration::days(30)))));
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-2", "Alpha Book", 1_600_000_000_000, Some((600.0, false, now - chrono::Duration::days(1)))));
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-3", "Middle Book", 1_500_000_000_000, None));

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(2).is_some(), Duration::from_secs(10));
        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Zed Book", "Alpha Book", "Middle Book"], "default sort is date-added descending");

        hooks.sort_buttons.last_listened.emit_clicked();
        pump_until(
            || flow_box_titles(&hooks.flow_box) == vec!["Alpha Book".to_string(), "Zed Book".to_string(), "Middle Book".to_string()],
            Duration::from_secs(5),
        );
    }

    /// The "In progress only" filter, driven through both of its manual entry points (the
    /// popover's check and the banner's "Show all") plus the navigation path (`apply_view`,
    /// what Home's Continue Listening header triggers): every surface — banner, funnel icon,
    /// the check itself — must reflect the one shared state, whichever of them changed it.
    pub(crate) fn run_in_progress_filter_is_manually_toggleable(runtime: &tokio::runtime::Runtime) {
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
                        item_json("item-1", "Reading Now", "Author A", 1_700_000_000_000, 3600.0),
                        item_json("item-2", "Finished Book", "Author B", 1_600_000_000_000, 3600.0),
                        item_json("item-3", "Untouched Book", "Author C", 1_500_000_000_000, 3600.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        let now = chrono::Utc::now();
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-1", "Reading Now", 1_700_000_000_000, Some((600.0, false, now))));
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-2", "Finished Book", 1_600_000_000_000, Some((3600.0, true, now - chrono::Duration::days(2)))));
        runtime.block_on(seed_item_with_progress(&pool, &server.id, &account.id, "item-3", "Untouched Book", 1_500_000_000_000, None));

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(2).is_some(), Duration::from_secs(10));
        assert_eq!(flow_box_titles(&hooks.flow_box).len(), 3, "no filter is active initially");
        assert!(!hooks.progress_banner.reveals_child(), "the filter banner stays hidden while no filter is active");
        assert!(!hooks.in_progress_check.is_active());

        // Via the popover's check — the manual path.
        hooks.in_progress_check.set_active(true);
        pump_until(|| hooks.progress_banner.reveals_child(), Duration::from_secs(5));
        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Reading Now"], "only the unfinished item with progress survives the filter");
        assert!(hooks.in_progress_check.is_active());
        assert_eq!(hooks.sort_menu_button.icon_name().as_deref(), Some("funnel-symbolic"), "the header button signals the active filter, Nautilus-style");

        // Via the banner's "Show all" — the escape hatch.
        hooks.progress_show_all.emit_clicked();
        pump_until(|| hooks.flow_box.child_at_index(2).is_some(), Duration::from_secs(5));
        assert!(!hooks.progress_banner.reveals_child());
        assert!(!hooks.in_progress_check.is_active(), "the check reflects the shared state, not just the banner");
        assert_eq!(hooks.sort_menu_button.icon_name().as_deref(), Some("view-sort-descending-symbolic"));

        // Via navigation (`apply_view`) — the Continue Listening header's path. The externally-
        // set state must sync the check back the other way: check → state, state → check.
        screen.apply_view(SortKey::LastListened, true);
        pump_until(|| hooks.progress_banner.reveals_child(), Duration::from_secs(5));
        assert!(hooks.in_progress_check.is_active(), "apply_view must sync the popover's check to the externally-set state");
        assert_eq!(flow_box_titles(&hooks.flow_box), vec!["Reading Now"]);
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.banner.widget().reveals_child(), Duration::from_secs(10));

        assert!(hooks.banner.widget().reveals_child(), "a sync failure should show the banner");
        assert!(hooks.status_page.is_visible(), "with nothing cached yet, the empty state stays up too");
    }

    /// Dead session with cached data on the Library tab: the banner must swap its copy and offer
    /// "Log in again" — a retry can't fix a revoked session, and the Library tab is where a user
    /// browsing while the token dies will actually be.
    pub(crate) fn run_shows_login_again_in_the_banner_when_a_resync_is_rejected(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(MockServer::start());
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(abs_storage::repo::libraries::upsert(
            &pool,
            abs_storage::repo::libraries::UpsertLibrary {
                id: "lib-1",
                server_id: &server.id,
                name: "Audiobooks",
                media_type: "book",
                icon: None,
                display_order: 1,
            },
        ))
        .unwrap();

        let relogin_requested = Rc::new(std::cell::Cell::new(false));
        let on_relogin = {
            let relogin_requested = relogin_requested.clone();
            move || relogin_requested.set(true)
        };
        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, on_relogin);
        let hooks = screen.test_hooks();

        pump_until(|| hooks.banner.widget().reveals_child(), Duration::from_secs(10));

        assert_eq!(hooks.banner.title(), "Session expired — showing what's cached.");
        assert!(hooks.banner.action_visible(), "an auth failure must offer Log in again in the banner");
        assert!(hooks.status_page.is_visible(), "no synced items yet, so the status page stays up");
        assert!(!relogin_requested.get(), "merely showing the banner must not trigger a re-login");

        hooks.banner.action_button().emit_clicked();
        assert!(relogin_requested.get(), "the banner's login action must fire on_relogin");
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, on_play, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));

        let child = hooks.flow_box.child_at_index(0).unwrap();
        let button = child.child().and_then(|w| w.downcast::<gtk4::Button>().ok()).expect("flow box child wraps a button");
        button.emit_clicked();

        assert_eq!(played.borrow().len(), 1);
        assert_eq!(played.borrow()[0].item_id, "item-1");
    }

    pub(crate) fn run_list_view_toggle_switches_visible_container(runtime: &tokio::runtime::Runtime) {
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
                        item_json("item-2", "The Martian", "Andy Weir", 1_600_000_000_000, 7200.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));
        assert!(hooks.flow_box.is_visible(), "grid should be visible by default");
        assert!(!hooks.list_box.is_visible());

        hooks.view_toggle.set_active(true);
        pump_until(|| hooks.list_box.row_at_index(1).is_some(), Duration::from_secs(5));

        assert!(!hooks.flow_box.is_visible(), "switching to list mode should hide the grid");
        assert!(hooks.list_box.is_visible());
        assert_eq!(list_box_titles(&hooks.list_box).len(), 2);

        hooks.view_toggle.set_active(false);
        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(5));
        assert!(hooks.flow_box.is_visible(), "switching back to grid mode should show it again");
        assert!(!hooks.list_box.is_visible());
    }

    pub(crate) fn run_list_view_rows_show_title_and_subtitle(runtime: &tokio::runtime::Runtime) {
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));
        hooks.view_toggle.set_active(true);
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        let row = hooks.list_box.row_at_index(0).unwrap().downcast::<adw::ActionRow>().unwrap();
        assert_eq!(row.title(), "Project Hail Mary");
        assert_eq!(row.subtitle().unwrap(), "Andy Weir · 1.0h");
    }

    pub(crate) fn run_tapping_a_list_row_invokes_on_play(runtime: &tokio::runtime::Runtime) {
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, on_play, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));
        hooks.view_toggle.set_active(true);
        pump_until(|| hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        let row = hooks.list_box.row_at_index(0).unwrap().downcast::<adw::ActionRow>().unwrap();
        row.emit_by_name::<()>("activated", &[]);

        assert_eq!(played.borrow().len(), 1);
        assert_eq!(played.borrow()[0].item_id, "item-1");
    }

    pub(crate) fn run_search_and_sort_apply_in_list_mode_too(runtime: &tokio::runtime::Runtime) {
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(1).is_some(), Duration::from_secs(10));
        hooks.view_toggle.set_active(true);
        pump_until(|| hooks.list_box.row_at_index(1).is_some(), Duration::from_secs(5));

        assert_eq!(list_box_titles(&hooks.list_box), vec!["Zed Book", "Alpha Book"], "default sort is date-added descending");

        hooks.sort_buttons.title.emit_clicked();
        pump_until(|| list_box_titles(&hooks.list_box) == vec!["Alpha Book".to_string(), "Zed Book".to_string()], Duration::from_secs(5));

        hooks.search_entry.set_text("zed");
        pump_until(|| list_box_titles(&hooks.list_box) == vec!["Zed Book".to_string()], Duration::from_secs(5));
    }

    /// Building a second screen against the same pool simulates the next time this tab is opened
    /// (same local DB, fresh widgets) — the persisted mode should apply itself without the user
    /// having to click the toggle again.
    pub(crate) fn run_view_mode_is_remembered_across_screen_rebuilds(runtime: &tokio::runtime::Runtime) {
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

        let first_screen = build(pool.clone(), crate::test_support::test_paths(), server.clone(), account.clone(), abs_core::auth::Session::new(pool.clone(), &server, &account), |_| {}, || {});
        let first_hooks = first_screen.test_hooks();
        pump_until(|| first_hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));
        assert!(first_hooks.flow_box.is_visible(), "starts in grid mode with nothing persisted yet");

        first_hooks.view_toggle.set_active(true);
        pump_until(|| first_hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(5));

        // The toggle's save is a fire-and-forget `spawn_future_local` (and the list rows render
        // synchronously), so the rows appearing proves nothing about the write having committed.
        // Probe the persisted value on the same main context — a future queued after the save —
        // and only rebuild once it reads back List, so the second screen's load can't race the
        // first screen's save.
        let persisted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        glib::spawn_future_local({
            let pool = pool.clone();
            let persisted = persisted.clone();
            async move {
                loop {
                    if abs_core::settings::load_library_view_mode(&pool).await.ok() == Some(abs_core::settings::LibraryViewMode::List) {
                        persisted.store(true, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
            }
        });
        pump_until(|| persisted.load(std::sync::atomic::Ordering::SeqCst), Duration::from_secs(5));

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let second_screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let second_hooks = second_screen.test_hooks();
        pump_until(|| second_hooks.list_box.row_at_index(0).is_some(), Duration::from_secs(10));

        assert!(second_hooks.list_box.is_visible(), "a freshly built screen should restore the persisted List mode");
        assert!(!second_hooks.flow_box.is_visible());
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

        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(20));

        assert!(!hooks.status_page.is_visible(), "the live demo server has at least one item, so the empty state should clear");
        assert!(hooks.flow_box.child_at_index(0).is_some());
    }

    /// "Sync now" via the ⋯ menu, then pull-to-refresh — the two manual triggers share one sync
    /// cycle and one outcome toast (LB-12, mirroring Home's HT-10 scenarios). Each trigger's
    /// round-trip lands a distinct response, so every stage's render is its own observable. The
    /// gesture itself is driven by the scroller's own `edge-overshot` signal — the exact one the
    /// gesture listens to; the sandbox has no touch hardware.
    pub(crate) fn run_sync_now_and_pull_to_refresh(runtime: &tokio::runtime::Runtime) {
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
                .up_to_n_times(1)
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
                .up_to_n_times(1)
                .mount(&mock_server),
        );
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path("/api/libraries/e4bb1afb-4a4f-4dd6-8be0-e615d233185b/items"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [
                        item_json("item-1", "Project Hail Mary", "Andy Weir", 1_700_000_000_000, 3600.0),
                        item_json("item-2", "Dune", "Frank Herbert", 1_600_000_000_000, 7200.0),
                        item_json("item-3", "Red Mars", "Kim Stanley Robinson", 1_500_000_000_000, 10800.0)
                    ]
                })))
                .mount(&mock_server),
        );

        let pool = runtime.block_on(pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        let session = abs_core::auth::Session::new(pool.clone(), &server, &account);
        let screen = build(pool, crate::test_support::test_paths(), server, account, session, |_| {}, || {});
        let hooks = screen.test_hooks();

        let app_window = adw::ApplicationWindow::builder().build();
        app_window.set_content(Some(&screen.root));
        // Mapped like Home's manual-sync scenarios — the outcome toast needs the overlay mapped.
        app_window.present();
        pump_until(|| app_window.is_mapped(), Duration::from_secs(5));

        pump_until(|| hooks.flow_box.child_at_index(0).is_some(), Duration::from_secs(10));
        assert_eq!(flow_box_titles(&hooks.flow_box).len(), 1);
        assert!(
            !crate::test_support::any_label_reads(hooks.toast_overlay.upcast_ref(), "Sync complete"),
            "the automatic cycle reports through the banner/status page, not a toast"
        );

        // Each trigger's round-trip lands a distinct response (1 → 2 → 3 items, via the stacked
        // `up_to_n_times` mocks above), so every stage's render is its own observable.
        hooks.sync_now_button.emit_clicked();
        pump_until(|| flow_box_titles(&hooks.flow_box).len() == 2, Duration::from_secs(10));
        // The toast lands at the cycle's resolve step — after its cover-fetch stage — so wait
        // on it, don't assert it immediately.
        pump_until(
            || crate::test_support::any_label_reads(hooks.toast_overlay.upcast_ref(), "Sync complete"),
            Duration::from_secs(10),
        );
        assert!(
            crate::test_support::any_label_reads(hooks.toast_overlay.upcast_ref(), "Sync complete"),
            "the manual sync's completion toast must appear"
        );

        hooks.scroller.emit_by_name::<()>("edge-overshot", &[&gtk4::PositionType::Top]);
        pump_until(|| flow_box_titles(&hooks.flow_box).len() == 3, Duration::from_secs(10));
        pump_until(
            || crate::test_support::any_label_reads(hooks.toast_overlay.upcast_ref(), "Sync complete"),
            Duration::from_secs(10),
        );
        assert!(
            crate::test_support::any_label_reads(hooks.toast_overlay.upcast_ref(), "Sync complete"),
            "the pull's completion toast must appear too"
        );
    }
}
