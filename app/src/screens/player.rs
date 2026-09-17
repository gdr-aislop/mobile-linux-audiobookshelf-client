//! The full player screen — `docs/design/ui-spec.md`'s "Player — full" section: core transport,
//! a secondary row of speed/sleep-timer/chapters controls, and a header `⋯` menu for "Add
//! bookmark". Opened by `main_window` swapping window content in (there's no
//! `AdwNavigationView`/`AdwDialog` available at this crate's libadwaita ceiling, both v1.4+); the
//! down-chevron header button calls `on_collapse` to swap back.

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
    pub speed_button: gtk4::MenuButton,
    pub speed_label: gtk4::Label,
    pub speed_popover_box: gtk4::Box,
    pub sleep_timer_button: gtk4::MenuButton,
    pub sleep_timer_popover: gtk4::Popover,
    pub sleep_timer_popover_box: gtk4::Box,
    pub menu_button: gtk4::MenuButton,
    pub add_bookmark_button: gtk4::Button,
    pub toast_overlay: adw::ToastOverlay,
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

    // The spec's `⋯` menu is dropped down to just "Add bookmark" — speed and sleep timer live as
    // secondary-row buttons instead (see the scope decision in this plan). Plain
    // `GtkMenuButton`/`GtkPopover`/`GtkButton`, same convention as the other popovers on this
    // screen, not `GMenu`/`GAction`.
    let add_bookmark_button = gtk4::Button::builder().label("Add bookmark").css_classes(["flat"]).halign(gtk4::Align::Start).build();
    let menu_popover_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    menu_popover_box.append(&add_bookmark_button);
    let menu_popover = gtk4::Popover::builder().child(&menu_popover_box).build();
    let menu_button = gtk4::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text("More").popover(&menu_popover).build();
    header.pack_end(&menu_button);

    let cover = crate::widgets::cover_image::CoverImage::new(264);
    cover.widget().set_halign(gtk4::Align::Center);
    cover.widget().set_margin_top(14);
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

    // Secondary row: speed, sleep timer, chapters. `AdwBottomSheet`/popover-menu widgets from
    // libadwaita 1.4+ are unavailable at this crate's v1.2 ceiling, so every one of these is a
    // plain `GtkMenuButton` + `GtkPopover` holding a `GtkBox` of buttons — matching this
    // codebase's existing convention of wiring everything through direct `connect_clicked`
    // closures rather than `GMenu`/`GAction` models.
    const SPEED_PRESETS: [f64; 8] = [0.8, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

    let speed_label = gtk4::Label::new(Some("1.0×"));
    let speed_popover_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    let speed_popover = gtk4::Popover::builder().child(&speed_popover_box).build();
    let speed_button = gtk4::MenuButton::builder().child(&speed_label).tooltip_text("Playback speed").popover(&speed_popover).build();
    for speed in SPEED_PRESETS {
        let button = gtk4::Button::builder().label(format_speed(speed)).css_classes(["flat"]).build();
        button.connect_clicked({
            let controller = controller.clone();
            let speed_popover = speed_popover.clone();
            move |_| {
                controller.set_speed(speed);
                speed_popover.popdown();
            }
        });
        speed_popover_box.append(&button);
    }

    let sleep_timer_popover_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    let sleep_timer_popover = gtk4::Popover::builder().child(&sleep_timer_popover_box).build();
    let sleep_timer_button = gtk4::MenuButton::builder()
        .icon_name("preferences-system-time-symbolic")
        .tooltip_text("Sleep timer")
        .popover(&sleep_timer_popover)
        .build();
    type SleepTimerAction = Box<dyn Fn(&PlayerController)>;
    let sleep_timer_options: [(&str, SleepTimerAction); 5] = [
        ("Off", Box::new(|c: &PlayerController| c.cancel_sleep_timer())),
        ("15 minutes", Box::new(|c: &PlayerController| c.set_sleep_timer_minutes(15))),
        ("30 minutes", Box::new(|c: &PlayerController| c.set_sleep_timer_minutes(30))),
        ("45 minutes", Box::new(|c: &PlayerController| c.set_sleep_timer_minutes(45))),
        ("End of chapter", Box::new(|c: &PlayerController| c.set_sleep_timer_end_of_chapter())),
    ];
    for (label, apply) in sleep_timer_options {
        let button = gtk4::Button::builder().label(label).css_classes(["flat"]).build();
        button.connect_clicked({
            let controller = controller.clone();
            let sleep_timer_popover = sleep_timer_popover.clone();
            move |_| {
                apply(&controller);
                sleep_timer_popover.popdown();
            }
        });
        sleep_timer_popover_box.append(&button);
    }

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
    let secondary_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(18).halign(gtk4::Align::Center).margin_top(10).build();
    secondary_row.append(&speed_button);
    secondary_row.append(&sleep_timer_button);
    secondary_row.append(&chapters_button);

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .margin_start(28)
        .margin_end(28)
        .margin_bottom(24)
        .build();
    content.append(cover.widget());
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

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&root));

    add_bookmark_button.connect_clicked({
        let controller = controller.clone();
        let menu_popover = menu_popover.clone();
        let toast_overlay = toast_overlay.clone();
        move |_| {
            controller.add_bookmark();
            menu_popover.popdown();
            toast_overlay.add_toast(adw::Toast::new("Bookmark added"));
        }
    });

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
        let speed_label = speed_label.clone();
        let sleep_timer_button = sleep_timer_button.clone();
        let cover = cover.clone();
        move |snapshot: &PlayerSnapshot| {
            title_label.set_label(&snapshot.title);
            author_label.set_label(snapshot.author.as_deref().unwrap_or(""));
            author_label.set_visible(snapshot.author.is_some());
            cover.set_path(snapshot.cover_path.as_deref());
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

            speed_label.set_label(&format_speed(snapshot.speed));
            if snapshot.sleep_timer_active {
                sleep_timer_button.add_css_class("accent");
            } else {
                sleep_timer_button.remove_css_class("accent");
            }
        }
    };

    // Paint whatever's already playing immediately, rather than waiting for the next tick.
    if let Some(snapshot) = controller.snapshot() {
        update(&snapshot);
    }
    controller.set_full_update(update);

    PlayerScreen {
        root: toast_overlay.clone().upcast(),
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
            speed_button,
            speed_label,
            speed_popover_box,
            sleep_timer_button,
            sleep_timer_popover,
            sleep_timer_popover_box,
            menu_button,
            add_bookmark_button,
            toast_overlay,
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

/// Formats a playback speed for the speed button/popover — trims a trailing `.0` (`"1×"` reads
/// oddly for a speed control; `"1.0×"` is the convention every audiobook app uses) but keeps
/// fractional speeds like `1.25×` intact.
fn format_speed(speed: f64) -> String {
    if (speed.fract()).abs() < f64::EPSILON {
        format!("{speed:.1}×")
    } else {
        format!("{speed}×")
    }
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

        let controller = crate::player::PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
            1.0,
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
        let controller = crate::player::PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Multi-track Book".to_string(), author: None },
            1.0,
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
        let controller = crate::player::PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
            1.0,
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

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Clicking a speed preset should call
    /// through to the real backend and update both the controller's snapshot and the button's
    /// label.
    pub(crate) fn run_speed_popover_changes_playback_speed(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let screen = build(controller.clone(), PlaybackSettings::default(), || {});
        let hooks = screen.test_hooks();
        assert_eq!(hooks.speed_label.label(), "1.0×", "should start at the default speed");
        assert!(hooks.speed_button.popover().is_some(), "the speed button should open a popover");

        click_button_labeled(&hooks.speed_popover_box, "1.5×");
        assert_eq!(controller.snapshot().unwrap().speed, 1.5, "clicking a preset should change the real controller's speed");
        assert_eq!(hooks.speed_label.label(), "1.5×");

        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Exercises the real end-of-chapter
    /// deadline path (not a test-only backdoor): a very short first chapter, armed via the
    /// popover, should pause playback once the position crosses the chapter boundary.
    pub(crate) fn run_sleep_timer_end_of_chapter_pauses_at_the_boundary(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(crate::player::tests::mock_playable_item_with_chapters(
            &mock_server,
            "item-1",
            5,
            &[("Short Chapter", 0.0, 1.0), ("Rest", 1.0, 5.0)],
        ));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool, crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| !controller.chapters().is_empty(), Duration::from_secs(10));

        let screen = build(controller.clone(), PlaybackSettings::default(), || {});
        let hooks = screen.test_hooks();
        assert!(!hooks.sleep_timer_button.has_css_class("accent"), "no sleep timer armed yet");
        assert_eq!(hooks.sleep_timer_button.popover().as_ref(), Some(&hooks.sleep_timer_popover));

        click_button_labeled(&hooks.sleep_timer_popover_box, "End of chapter");
        assert!(controller.snapshot().unwrap().sleep_timer_active);
        assert!(hooks.sleep_timer_button.has_css_class("accent"), "the sleep timer button should show it's armed");

        pump_until(|| !controller.snapshot().unwrap().is_playing, Duration::from_secs(10));
        assert!(
            !controller.snapshot().unwrap().is_playing,
            "reaching the end of the current chapter should pause playback"
        );
        assert!(!controller.snapshot().unwrap().sleep_timer_active, "firing should also disarm the timer");

        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. `AdwToastOverlay` has no public way
    /// to inspect a queued toast headless, so this checks the load-bearing effect instead: the
    /// menu button opens a popover, and clicking "Add bookmark" in it writes a row to storage.
    pub(crate) fn run_add_bookmark_button_persists_a_row(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            server,
            account,
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let screen = build(controller.clone(), PlaybackSettings::default(), || {});
        let hooks = screen.test_hooks();
        assert!(hooks.menu_button.popover().is_some(), "the ... menu button should open a popover");

        hooks.add_bookmark_button.emit_clicked();
        pump_until(|| false, Duration::from_millis(300));

        let count: i64 =
            runtime.block_on(sqlx::query_scalar("SELECT COUNT(*) FROM bookmarks").fetch_one(&pool)).unwrap();
        assert_eq!(count, 1, "clicking Add bookmark should persist a row");
        assert!(hooks.toast_overlay.child().is_some(), "the toast overlay should still be hosting the screen content");
        controller.stop();
    }

    /// Finds and clicks the button with the given label inside a popover's button box (speed or
    /// sleep-timer presets), so tests can drive the popover the same way a user tapping it would.
    fn click_button_labeled(container: &gtk4::Box, label: &str) {
        let mut child = container.first_child();
        while let Some(widget) = child {
            if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
                if button.label().as_deref() == Some(label) {
                    button.emit_clicked();
                    return;
                }
            }
            child = widget.next_sibling();
        }
        panic!("no button labeled {label:?} found");
    }

    /// Extracts the title label's text from a chapters-sheet row built by `build_chapter_row`.
    fn row_title(row: &gtk4::ListBoxRow) -> adw::glib::GString {
        let row_box = row.child().expect("row has a child box").downcast::<gtk4::Box>().unwrap();
        let title_label = row_box.first_child().expect("row box has a title label").downcast::<gtk4::Label>().unwrap();
        title_label.label()
    }
}
