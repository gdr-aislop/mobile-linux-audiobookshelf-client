//! The full player screen — `docs/design/ui-spec.md`'s "Player — full" section. Core transport
//! plus a chapters sheet this pass; speed control, sleep timer, and bookmarks are still to come.
//! Opened by `main_window` swapping window content in (there's no `AdwNavigationView`/`AdwDialog`
//! available at this crate's libadwaita ceiling, both v1.4+); the down-chevron header button calls
//! `on_collapse` to swap back.

use adw::prelude::*;

use abs_core::settings::PlaybackSettings;

use crate::player::{ChapterInfo, PlayerController, PlayerSnapshot};

pub struct PlayerScreen {
    pub root: gtk4::Widget,
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
pub struct TestHooks {
    pub title_label: gtk4::Label,
    pub author_label: gtk4::Label,
    pub play_button: gtk4::Button,
    pub collapse_button: gtk4::Button,
    pub scrubber: gtk4::Scale,
    pub elapsed_label: gtk4::Label,
    pub remaining_label: gtk4::Label,
    pub multi_track_label: gtk4::Label,
    pub chapters_button: gtk4::MenuButton,
    pub chapters_popover: gtk4::Popover,
    pub chapters_list: gtk4::ListBox,
}

#[cfg(test)]
impl PlayerScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

pub fn build(controller: PlayerController, playback_settings: PlaybackSettings, on_collapse: impl Fn() + 'static) -> PlayerScreen {
    let header = adw::HeaderBar::new();
    let collapse_button = gtk4::Button::from_icon_name("go-down-symbolic");
    collapse_button.connect_clicked({
        let controller = controller.clone();
        move |_| {
            controller.clear_full_update();
            on_collapse();
        }
    });
    header.pack_start(&collapse_button);
    header.set_title_widget(Some(&adw::WindowTitle::new("Now Playing", "")));

    let cover = gtk4::Box::builder()
        .css_classes(["card"])
        .width_request(264)
        .height_request(264)
        .halign(gtk4::Align::Center)
        .margin_top(14)
        .build();
    let title_label = gtk4::Label::builder()
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .css_classes(["title-2"])
        .margin_top(22)
        .build();
    let author_label =
        gtk4::Label::builder().wrap(true).justify(gtk4::Justification::Center).css_classes(["dim-label"]).margin_top(4).build();

    let scrubber = gtk4::Scale::builder().orientation(gtk4::Orientation::Horizontal).hexpand(true).build();
    scrubber.set_range(0.0, 1.0);
    scrubber.set_draw_value(false);
    let elapsed_label = gtk4::Label::builder().xalign(0.0).css_classes(["caption", "dim-label"]).build();
    let remaining_label = gtk4::Label::builder().xalign(1.0).css_classes(["caption", "dim-label"]).build();
    let time_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).margin_top(8).build();
    elapsed_label.set_hexpand(true);
    remaining_label.set_hexpand(true);
    time_row.append(&elapsed_label);
    time_row.append(&remaining_label);

    let skip_back = gtk4::Button::builder()
        .css_classes(["circular"])
        .child(&gtk4::Image::from_icon_name("media-seek-backward-symbolic"))
        .build();
    let play_button = gtk4::Button::builder()
        .css_classes(["circular", "suggested-action"])
        .width_request(72)
        .height_request(72)
        .child(&gtk4::Image::from_icon_name("media-playback-pause-symbolic"))
        .build();
    let skip_forward = gtk4::Button::builder()
        .css_classes(["circular"])
        .child(&gtk4::Image::from_icon_name("media-seek-forward-symbolic"))
        .build();
    let transport = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(30).halign(gtk4::Align::Center).margin_top(26).build();
    transport.append(&skip_back);
    transport.append(&play_button);
    transport.append(&skip_forward);

    let multi_track_label = gtk4::Label::builder()
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .css_classes(["caption", "dim-label"])
        .margin_top(14)
        .visible(false)
        .build();

    // Secondary row: chapters this pass (speed control and sleep timer land here in later
    // passes). `AdwBottomSheet` is v1.6+, unavailable at this crate's v1.2 ceiling, so the
    // chapters sheet is a plain `GtkPopover` — a `GtkScrolledWindow` caps its height so a long
    // chapter list scrolls instead of forcing the popover to fill the screen.
    let chapters_list = gtk4::ListBox::builder().selection_mode(gtk4::SelectionMode::None).css_classes(["boxed-list"]).build();
    let chapters_scroller = gtk4::ScrolledWindow::builder()
        .max_content_height(320)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .width_request(260)
        .child(&chapters_list)
        .build();
    let chapters_popover = gtk4::Popover::builder().child(&chapters_scroller).build();
    let chapters_button = gtk4::MenuButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Chapters")
        .popover(&chapters_popover)
        .build();
    let secondary_row =
        gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).halign(gtk4::Align::Center).margin_top(10).build();
    secondary_row.append(&chapters_button);

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .margin_start(28)
        .margin_end(28)
        .margin_bottom(24)
        .build();
    content.append(&cover);
    content.append(&title_label);
    content.append(&author_label);
    content.append(&scrubber);
    content.append(&time_row);
    content.append(&transport);
    content.append(&secondary_row);
    content.append(&multi_track_label);

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&content);

    let skip_back_seconds = playback_settings.skip_back_seconds as f64;
    let skip_forward_seconds = playback_settings.skip_forward_seconds as f64;

    skip_back.connect_clicked({
        let controller = controller.clone();
        move |_| controller.skip(-skip_back_seconds)
    });
    skip_forward.connect_clicked({
        let controller = controller.clone();
        move |_| controller.skip(skip_forward_seconds)
    });
    play_button.connect_clicked({
        let controller = controller.clone();
        move |_| controller.toggle_play_pause()
    });

    // Populated fresh every time the popover is about to open (not once at screen-build time), so
    // "current chapter highlighted" always reflects the position at the moment it's opened.
    chapters_popover.connect_show({
        let controller = controller.clone();
        let chapters_list = chapters_list.clone();
        move |_| {
            while let Some(child) = chapters_list.first_child() {
                chapters_list.remove(&child);
            }
            let chapters = controller.chapters();
            let position = controller.snapshot().map(|s| s.position_seconds).unwrap_or(0.0);
            for chapter in &chapters {
                chapters_list.append(&build_chapter_row(chapter, position));
            }
        }
    });
    chapters_list.connect_row_activated({
        let controller = controller.clone();
        let chapters_popover = chapters_popover.clone();
        move |_, row| {
            if let Some(chapter) = controller.chapters().get(row.index() as usize) {
                controller.seek_to_seconds(chapter.start_seconds);
            }
            chapters_popover.popdown();
        }
    });

    // `value-changed` fires both for user drags and for our own `scrubber.set_value(...)` calls
    // from `update()` below — a `RefCell` guard distinguishes the two so a live position update
    // doesn't get immediately re-interpreted as the user dragging (which would fight the real
    // playback position every tick).
    let updating_from_snapshot = std::rc::Rc::new(std::cell::Cell::new(false));
    scrubber.connect_value_changed({
        let controller = controller.clone();
        let updating_from_snapshot = updating_from_snapshot.clone();
        move |scale| {
            if updating_from_snapshot.get() {
                return;
            }
            controller.seek_fraction(scale.value());
        }
    });

    let update = {
        let title_label = title_label.clone();
        let author_label = author_label.clone();
        let play_button = play_button.clone();
        let scrubber = scrubber.clone();
        let elapsed_label = elapsed_label.clone();
        let remaining_label = remaining_label.clone();
        let multi_track_label = multi_track_label.clone();
        move |snapshot: &PlayerSnapshot| {
            title_label.set_label(&snapshot.title);
            author_label.set_label(snapshot.author.as_deref().unwrap_or(""));
            author_label.set_visible(snapshot.author.is_some());
            play_button.set_child(Some(&gtk4::Image::from_icon_name(if snapshot.is_playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            })));

            let fraction = if snapshot.duration_seconds > 0.0 {
                (snapshot.position_seconds / snapshot.duration_seconds).clamp(0.0, 1.0)
            } else {
                0.0
            };
            updating_from_snapshot.set(true);
            scrubber.set_value(fraction);
            updating_from_snapshot.set(false);

            elapsed_label.set_label(&format_hms(snapshot.position_seconds));
            remaining_label.set_label(&format!("-{}", format_hms((snapshot.duration_seconds - snapshot.position_seconds).max(0.0))));

            match &snapshot.multi_track_note {
                Some(note) => {
                    multi_track_label.set_label(note);
                    multi_track_label.set_visible(true);
                }
                None => multi_track_label.set_visible(false),
            }
        }
    };

    // Paint whatever's already playing immediately, rather than waiting for the next tick.
    if let Some(snapshot) = controller.snapshot() {
        update(&snapshot);
    }
    controller.set_full_update(update);

    PlayerScreen {
        root: root.upcast(),
        #[cfg(test)]
        hooks: TestHooks {
            title_label,
            author_label,
            play_button,
            collapse_button,
            scrubber,
            elapsed_label,
            remaining_label,
            multi_track_label,
            chapters_button,
            chapters_popover,
            chapters_list,
        },
    }
}

/// One row in the chapters sheet: title on the left, start time on the right, highlighted (via a
/// css class) if `position` currently falls within this chapter's range.
fn build_chapter_row(chapter: &ChapterInfo, position: f64) -> gtk4::ListBoxRow {
    let is_current = chapter.start_seconds <= position && position < chapter.end_seconds;

    let title_label = gtk4::Label::builder().label(&chapter.title).xalign(0.0).hexpand(true).ellipsize(gtk4::pango::EllipsizeMode::End).build();
    let time_label = gtk4::Label::builder().label(format_hms(chapter.start_seconds)).css_classes(["dim-label"]).build();
    let row_box = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).margin_top(6).margin_bottom(6).build();
    row_box.append(&title_label);
    row_box.append(&time_label);

    if is_current {
        title_label.add_css_class("heading");
    }

    gtk4::ListBoxRow::builder().child(&row_box).build()
}

fn format_hms(total_seconds: f64) -> String {
    let total_seconds = total_seconds.max(0.0) as u64;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::player::tests::{account_and_server, insert_synced_item, mock_playable_item, test_backend};
    use crate::player::PlayRequest;
    use crate::test_support::pump_until;
    use std::time::Duration;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point. Starts real playback
    /// (reusing `player.rs`'s own wiremock-served-audio test helpers), builds the full player
    /// screen against the already-playing controller, and checks its widgets reflect real state.
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool, test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let collapsed = std::rc::Rc::new(std::cell::Cell::new(false));
        let screen = build(controller.clone(), PlaybackSettings::default(), {
            let collapsed = collapsed.clone();
            move || collapsed.set(true)
        });
        let hooks = screen.test_hooks();

        // The screen paints from `controller.snapshot()` immediately on build, before any tick.
        assert_eq!(hooks.title_label.label(), "Test Book");
        assert_eq!(hooks.author_label.label(), "Some Author");

        // Let a tick or two land so the scrubber/labels reflect a live position.
        pump_until(|| hooks.scrubber.value() >= 0.0, Duration::from_millis(500));
        assert!(!hooks.elapsed_label.label().is_empty());
        assert!(hooks.remaining_label.label().starts_with('-'), "remaining time should count down");

        hooks.play_button.emit_clicked();
        pump_until(|| !controller.snapshot().unwrap().is_playing, Duration::from_secs(5));
        assert!(!controller.snapshot().unwrap().is_playing, "the full player's play/pause button should control the real controller");

        hooks.collapse_button.emit_clicked();
        assert!(collapsed.get(), "the down-chevron should call on_collapse");
        controller.stop();
    }

    pub(crate) fn run_multi_track_caveat_is_shown(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(async {
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/items/item-1"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "media": { "audioFiles": [
                        { "ino": "1", "duration": 5.0 },
                        { "ino": "2", "duration": 5.0 },
                    ] }
                })))
                .mount(&mock_server)
                .await;
        });

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        // No `/file/1` mock is needed for this test — the point is the caveat text, which is
        // computed from `get_item_playback_info`'s response alone, before any audio ever loads.
        let controller = crate::player::PlayerController::new(pool, test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let screen = build(controller.clone(), PlaybackSettings::default(), || {});
        let hooks = screen.test_hooks();

        assert!(hooks.multi_track_label.is_visible());
        assert!(hooks.multi_track_label.label().contains('2'));
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Seeds a two-chapter item, opens the
    /// chapters popover, checks both rows are listed with the right titles, then activates the
    /// second row and checks the controller actually seeks to its start.
    pub(crate) fn run_chapters_sheet_lists_and_seeks(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(crate::player::tests::mock_playable_item_with_chapters(
            &mock_server,
            "item-1",
            10,
            &[("Intro", 0.0, 4.0), ("Chapter One", 4.0, 10.0)],
        ));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        // Tap-to-seek needs real, seekable audio — unlike the multi-track caveat test above,
        // which only needs `get_item_playback_info`'s response, not real playback.
        let controller = crate::player::PlayerController::new(pool, test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
        );
        pump_until(|| !controller.chapters().is_empty(), Duration::from_secs(10));
        // Give the pipeline a moment to actually become seekable (same "position() becomes Some"
        // readiness signal `PlayerController::start` itself waits on for the resume-seek).
        pump_until(|| controller.snapshot().unwrap().position_seconds > 0.0, Duration::from_secs(5));

        let screen = build(controller.clone(), PlaybackSettings::default(), || {});
        let hooks = screen.test_hooks();
        assert_eq!(hooks.chapters_button.popover().as_ref(), Some(&hooks.chapters_popover), "the chapters button should open the chapters popover");

        // A `GtkPopover` needs a realized (mapped) toplevel ancestor to actually pop up — unlike
        // every other widget this test suite checks, which only need to exist, not be shown.
        let window = gtk4::Window::builder().child(&screen.root).build();
        window.present();
        pump_until(|| window.is_mapped(), Duration::from_secs(5));

        hooks.chapters_popover.popup();
        pump_until(|| hooks.chapters_list.row_at_index(1).is_some(), Duration::from_secs(2));

        let row0 = hooks.chapters_list.row_at_index(0).expect("first chapter row");
        let row1 = hooks.chapters_list.row_at_index(1).expect("second chapter row");
        assert!(hooks.chapters_list.row_at_index(2).is_none(), "should list exactly the two chapters");
        assert_eq!(row_title(&row0), "Intro");
        assert_eq!(row_title(&row1), "Chapter One");

        hooks.chapters_list.emit_by_name::<()>("row-activated", &[&row1]);
        pump_until(|| controller.snapshot().unwrap().position_seconds >= 4.0, Duration::from_secs(5));
        assert!(
            controller.snapshot().unwrap().position_seconds >= 4.0,
            "activating the second chapter's row should seek to its start"
        );

        window.destroy();
        controller.stop();
    }

    /// Extracts the title label's text from a chapters-sheet row built by `build_chapter_row`.
    fn row_title(row: &gtk4::ListBoxRow) -> adw::glib::GString {
        let row_box = row.child().expect("row has a child box").downcast::<gtk4::Box>().unwrap();
        let title_label = row_box.first_child().expect("row box has a title label").downcast::<gtk4::Label>().unwrap();
        title_label.label()
    }
}
