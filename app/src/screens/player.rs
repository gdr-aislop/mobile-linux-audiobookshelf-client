//! The full player screen — `docs/design/ui-spec.md`'s "Player — full" section: core transport,
//! a secondary row of speed/sleep-timer/chapters controls, and a header `⋯` menu for "Add
//! bookmark", "Mark as finished", and "Reset progress". Opened by `main_window` swapping window
//! content in (there's no `AdwNavigationView`/`AdwDialog` available at this crate's libadwaita
//! ceiling, both v1.4+); the down-chevron header button calls `on_collapse` to swap back.
//!
//! The keyboard-only equivalents of the on-screen controls (ui-spec §6's full-player set:
//! arrow-key skip, speed stepping, `c`/`t`/Escape) are registered into the `SimpleActionGroup`
//! exposed on `PlayerScreen` — `main_window` merges it under the "player" action prefix for as
//! long as this screen is open and removes it on collapse, so those accelerators are inert
//! everywhere else.

use std::rc::Rc;

use adw::glib;
use adw::prelude::*;

use abs_core::playback::SPEED_PRESETS;

use crate::downloads::DownloadManager;
use crate::player::{ChapterInfo, PlayerController, PlayerSnapshot};

pub struct PlayerScreen {
    pub root: gtk4::Widget,
    /// The screen's keyboard actions, named per ui-spec §6 (`skip-back`, `skip-forward`,
    /// `speed-up`, `speed-down`, `speed-reset`, `chapters`, `sleep-timer`, `collapse`). The
    /// window merges this group under the "player" prefix while the screen is open — see the
    /// module docs.
    pub actions: gtk4::gio::SimpleActionGroup,
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
    pub mark_as_finished_button: gtk4::Button,
    pub reset_progress_button: gtk4::Button,
    pub toast_overlay: adw::ToastOverlay,
    pub download_button: gtk4::MenuButton,
    pub download_popover: gtk4::Popover,
    pub download_popover_box: gtk4::Box,
}

#[cfg(test)]
impl PlayerScreen {
    pub fn test_hooks(&self) -> &TestHooks {
        &self.hooks
    }
}

pub fn build(
    pool: sqlx::SqlitePool,
    controller: PlayerController,
    download_manager: DownloadManager,
    on_collapse: impl Fn() + 'static,
) -> PlayerScreen {
    // Shared by the down-chevron header button and the Escape action below.
    let on_collapse = Rc::new(on_collapse);
    let header = adw::HeaderBar::new();
    let collapse_button = gtk4::Button::from_icon_name("go-down-symbolic");
    collapse_button.connect_clicked({
        let controller = controller.clone();
        let on_collapse = on_collapse.clone();
        move |_| {
            controller.clear_full_update();
            on_collapse();
        }
    });
    header.pack_start(&collapse_button);
    header.set_title_widget(Some(&adw::WindowTitle::new("Now Playing", "")));

    // The spec's `⋯` menu is dropped down to "Add bookmark" plus two testing/recovery actions —
    // speed and sleep timer live as secondary-row buttons instead (see the scope decision in this
    // plan). Plain `GtkMenuButton`/`GtkPopover`/`GtkButton`, same convention as the other popovers
    // on this screen, not `GMenu`/`GAction`. "Reset progress" is styled destructive (it throws
    // away the current listening position) but, matching the spec's own precedent for "Clear
    // downloaded chapters", isn't behind a confirmation dialog — `AdwAlertDialog` isn't available
    // at this crate's libadwaita v1.2 ceiling anyway, and being tucked inside a secondary menu is
    // enough friction for something this recoverable (nothing about the book itself is deleted).
    let add_bookmark_button = gtk4::Button::builder().label("Add bookmark").css_classes(["flat"]).halign(gtk4::Align::Start).build();
    let mark_as_finished_button = gtk4::Button::builder().label("Mark as finished").css_classes(["flat"]).halign(gtk4::Align::Start).build();
    // `destructive-action` combined with `flat` renders invisible (background-matching) text in
    // this popover's context — confirmed live: the button worked when clicked, but its label was
    // blank. `destructive-action` is meant for a solid filled button, not a flat text row, so
    // this one skips `flat` and gets its own margin to read as a distinct, deliberately separate
    // action rather than a fourth identical-looking menu row.
    let reset_progress_button = gtk4::Button::builder().label("Reset progress").css_classes(["destructive-action"]).margin_top(6).build();
    let menu_popover_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    menu_popover_box.append(&add_bookmark_button);
    menu_popover_box.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    menu_popover_box.append(&mark_as_finished_button);
    menu_popover_box.append(&reset_progress_button);
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

    // Secondary row: speed, sleep timer, chapters. `AdwBottomSheet`/popover-menu widgets from
    // libadwaita 1.4+ are unavailable at this crate's v1.2 ceiling, so every one of these is a
    // plain `GtkMenuButton` + `GtkPopover` holding a `GtkBox` of buttons — matching this
    // codebase's existing convention of wiring everything through direct `connect_clicked`
    // closures rather than `GMenu`/`GAction` models.

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
    // "downloaded" legend (ui-spec: sits in the Chapters section header so the downloaded
    // glyph's meaning doesn't need to be inferred from a single unlabeled icon on the active
    // chapters) — the same folder-download glyph the chapter rows and the covers' downloaded
    // badge use, so the icon itself is the legend.
    let chapters_legend = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(4).margin_bottom(4).build();
    chapters_legend.append(&gtk4::Image::builder().icon_name("folder-download-symbolic").css_classes(["dim-label"]).build());
    chapters_legend.append(&gtk4::Label::builder().label("downloaded").css_classes(["caption", "dim-label"]).build());
    let chapters_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    chapters_box.append(&chapters_legend);
    chapters_box.append(&chapters_list);
    let chapters_scroller = gtk4::ScrolledWindow::builder()
        .max_content_height(320)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .width_request(260)
        .child(&chapters_box)
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

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&content);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&root));

    // The download button + its scope popover — shared with Item Detail's own
    // (`widgets::download_scope_menu`; see its module doc for why this moved out of this file).
    // `current_download_context()` is read once, here, rather than inside a closure: a
    // `PlayerScreen` is built fresh on every mini-bar tap, so there is always something already
    // playing by the time this runs — unlike `chapter_ranges`/`current_chapter_index`/free space
    // below, which the widget itself re-asks on every popover open since those genuinely change
    // over a session's lifetime.
    let (download_session, _download_server_id, download_item_id) =
        controller.current_download_context().expect("a player screen is only ever built once something is playing");
    let download_menu = crate::widgets::download_scope_menu::build(
        pool.clone(),
        download_manager.clone(),
        download_session,
        download_item_id,
        {
            let controller = controller.clone();
            move || controller.chapters().iter().map(|c| (c.start_seconds, c.end_seconds)).collect()
        },
        {
            let controller = controller.clone();
            move || controller.current_chapter_index().unwrap_or(0)
        },
        {
            let download_manager = download_manager.clone();
            move || download_manager.free_space_bytes()
        },
        toast_overlay.clone(),
    );
    secondary_row.append(&download_menu.widget);

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
    mark_as_finished_button.connect_clicked({
        let controller = controller.clone();
        let menu_popover = menu_popover.clone();
        let toast_overlay = toast_overlay.clone();
        move |_| {
            controller.mark_as_finished();
            menu_popover.popdown();
            toast_overlay.add_toast(adw::Toast::new("Marked as finished"));
        }
    });
    reset_progress_button.connect_clicked({
        let controller = controller.clone();
        let menu_popover = menu_popover.clone();
        let toast_overlay = toast_overlay.clone();
        move |_| {
            controller.reset_progress();
            menu_popover.popdown();
            toast_overlay.add_toast(adw::Toast::new("Progress reset"));
        }
    });

    // The skip intervals are read at click time from the controller (Settings → Playback's live
    // config) rather than captured from `playback_settings` at build — a Settings edit applies to
    // the very next skip, here and in the keyboard actions below.
    skip_back.connect_clicked({
        let controller = controller.clone();
        move |_| controller.skip(-controller.skip_intervals().0)
    });
    skip_forward.connect_clicked({
        let controller = controller.clone();
        move |_| controller.skip(controller.skip_intervals().1)
    });
    play_button.connect_clicked({
        let controller = controller.clone();
        move |_| controller.toggle_play_pause()
    });

    // Keyboard actions (ui-spec §6's full-player table), registered into the group the main
    // window merges under the "player" prefix. Every action mirrors a button on this screen —
    // the arrow keys reuse the transport buttons' skip intervals, the speed keys step through
    // `SPEED_PRESETS` relative to the current speed, and `c`/`t` pop the same popovers their
    // menu buttons open.
    let actions = gtk4::gio::SimpleActionGroup::new();
    add_action(&actions, "skip-back", { let controller = controller.clone(); move || controller.skip(-controller.skip_intervals().0) });
    add_action(&actions, "skip-forward", { let controller = controller.clone(); move || controller.skip(controller.skip_intervals().1) });

    // Speed steps are relative to whatever is current, so they read as "next/previous preset" no
    // matter where in the list the user is — including from a speed that came straight from the
    // Settings default and isn't itself a preset. At either end of the list the step is a no-op.
    let step_speed = {
        let controller = controller.clone();
        move |up: bool| {
            let current = controller.snapshot().map(|s| s.speed).unwrap_or(1.0);
            let target = if up {
                SPEED_PRESETS.iter().copied().find(|p| *p > current + f64::EPSILON)
            } else {
                SPEED_PRESETS.iter().copied().rev().find(|p| *p < current - f64::EPSILON)
            };
            if let Some(speed) = target {
                controller.set_speed(speed);
            }
        }
    };
    add_action(&actions, "speed-up", { let step_speed = step_speed.clone(); move || step_speed(true) });
    add_action(&actions, "speed-down", move || step_speed(false));
    add_action(&actions, "speed-reset", {
        let controller = controller.clone();
        move || {
            // Guarding the already-at-1× case skips `set_speed`'s redundant seek-with-rate (the
            // same reasoning `start()` applies before re-applying the default speed).
            if controller.snapshot().is_some_and(|s| (s.speed - 1.0).abs() > f64::EPSILON) {
                controller.set_speed(1.0);
            }
        }
    });
    add_action(&actions, "chapters", { let popover = chapters_popover.clone(); move || popover.popup() });
    add_action(&actions, "sleep-timer", { let popover = sleep_timer_popover.clone(); move || popover.popup() });
    add_action(&actions, "collapse", {
        let controller = controller.clone();
        let on_collapse = on_collapse.clone();
        move || {
            controller.clear_full_update();
            on_collapse();
        }
    });

    // Populated fresh every time the popover is about to open (not once at screen-build time), so
    // "current chapter highlighted" always reflects the position at the moment it's opened.
    chapters_popover.connect_show({
        let controller = controller.clone();
        let chapters_list = chapters_list.clone();
        let pool = pool.clone();
        move |_| {
            let chapters = controller.chapters();
            let position = controller.snapshot().map(|s| s.position_seconds).unwrap_or(0.0);
            rebuild_chapters_list(&chapters_list, &chapters, position, &[]);

            // Offline markers need an async DB read (`complete_inos_for_item`), unlike everything
            // else here (in-memory on `controller`) — render without them first, then refine once
            // the query lands, same "show now, refine once the async bit lands" shape `home.rs`'s
            // cover-art re-render already uses.
            if let Some((_, server_id, item_id)) = controller.current_download_context() {
                let pool = pool.clone();
                let chapters_list = chapters_list.clone();
                let controller = controller.clone();
                glib::spawn_future_local(async move {
                    let chapters = controller.chapters();
                    if chapters.is_empty() {
                        return;
                    }
                    let chapter_ranges: Vec<(f64, f64)> = chapters.iter().map(|c| (c.start_seconds, c.end_seconds)).collect();
                    let markers = abs_core::download_tracks::chapter_offline_markers_for_item(&pool, &server_id, &item_id, &chapter_ranges).await.unwrap_or_default();
                    let position = controller.snapshot().map(|s| s.position_seconds).unwrap_or(0.0);
                    rebuild_chapters_list(&chapters_list, &chapters, position, &markers);
                });
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
        actions,
        #[cfg(test)]
        hooks: TestHooks {
            title_label,
            author_label,
            play_button,
            collapse_button,
            scrubber,
            elapsed_label,
            remaining_label,
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
            mark_as_finished_button,
            reset_progress_button,
            toast_overlay,
            #[cfg(test)]
            download_button: download_menu.widget,
            #[cfg(test)]
            download_popover: download_menu.popover,
            #[cfg(test)]
            download_popover_box: download_menu.popover_box,
        },
    }
}

/// Registers one of the full player's keyboard actions (ui-spec §6) as a stateless
/// `SimpleAction` on the screen's action group, calling `run` on every activation.
fn add_action(group: &gtk4::gio::SimpleActionGroup, name: &'static str, run: impl Fn() + 'static) {
    let action = gtk4::gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| run());
    group.add_action(&action);
}

/// Clears and repopulates `chapters_list`. `markers[i]` (if present for index `i`) shows a small
/// offline glyph trailing chapter `i`'s time — an empty slice (the synchronous first pass, before
/// the async offline lookup lands) means "not downloaded" for every chapter, never "unknown"; a
/// glyph appearing a moment later is the expected two-stage render, not a flicker to hide.
fn rebuild_chapters_list(chapters_list: &gtk4::ListBox, chapters: &[ChapterInfo], position: f64, markers: &[bool]) {
    while let Some(child) = chapters_list.first_child() {
        chapters_list.remove(&child);
    }
    for (index, chapter) in chapters.iter().enumerate() {
        let is_downloaded = markers.get(index).copied().unwrap_or(false);
        chapters_list.append(&build_chapter_row(chapter, position, is_downloaded));
    }
}

/// One row in the chapters sheet: title on the left, start time (plus a small offline glyph when
/// `is_downloaded`) on the right, highlighted (via a css class) if `position` currently falls
/// within this chapter's range.
fn build_chapter_row(chapter: &ChapterInfo, position: f64, is_downloaded: bool) -> gtk4::ListBoxRow {
    let is_current = chapter.start_seconds <= position && position < chapter.end_seconds;

    let title_label = gtk4::Label::builder().label(&chapter.title).xalign(0.0).hexpand(true).ellipsize(gtk4::pango::EllipsizeMode::End).build();
    let time_label = gtk4::Label::builder().label(format_hms(chapter.start_seconds)).css_classes(["dim-label"]).build();
    let row_box = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).margin_top(6).margin_bottom(6).build();
    row_box.append(&title_label);
    row_box.append(&time_label);
    if is_downloaded {
        // Same glyph as the covers' downloaded badge — a "this is on disk and playable offline"
        // marker, not a completion checkmark (which is the download *button's* state icon).
        row_box.append(&gtk4::Image::builder().icon_name("folder-download-symbolic").css_classes(["dim-label"]).tooltip_text("Downloaded").build());
    }

    if is_current {
        title_label.add_css_class("heading");
    }

    gtk4::ListBoxRow::builder().child(&row_box).build()
}

/// Formats a playback speed for the speed button/popover — trims a trailing `.0` (`"1×"` reads
/// oddly for a speed control; `"1.0×"` is the convention every audiobook app uses) but keeps
/// fractional speeds like `1.25×` intact. Shared with Settings' "Default speed" row, which lists
/// the same presets.
pub(crate) fn format_speed(speed: f64) -> String {
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

    fn test_download_manager(pool: sqlx::SqlitePool) -> DownloadManager {
        DownloadManager::new(pool, crate::test_support::test_paths(), Box::new(abs_player::network_watch::UnknownNetworkMonitor), false)
    }

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

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: Some("Some Author".to_string()) },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let collapsed = std::rc::Rc::new(std::cell::Cell::new(false));
        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), {
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

        // Tap-to-seek needs real, seekable audio, not just a parsed `get_item_playback_info`
        // response — hence `mock_playable_item_with_chapters` and the readiness wait below.
        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| !controller.chapters().is_empty(), Duration::from_secs(10));
        // Give the pipeline a moment to actually become seekable (same "position() becomes Some"
        // readiness signal `PlayerController::start` itself waits on for the resume-seek).
        pump_until(|| controller.snapshot().unwrap().position_seconds > 0.0, Duration::from_secs(5));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
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

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Tapping "Current chapter" in the
    /// download popover should call through to a real download (via `DownloadManager`) for that
    /// chapter's track only, and the button's icon should reflect Downloading -> Complete as the
    /// real `DownloadEvent`s land.
    pub(crate) fn run_download_button_starts_a_download_and_reflects_state(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(crate::player::tests::mock_playable_item_with_chapters(&mock_server, "item-1", 10, &[("Intro", 0.0, 4.0), ("Chapter One", 4.0, 10.0)]));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| !controller.chapters().is_empty(), Duration::from_secs(10));

        let download_manager = test_download_manager(pool.clone());
        let screen = build(pool.clone(), controller.clone(), download_manager, || {});
        let hooks = screen.test_hooks();

        let window = gtk4::Window::builder().child(&screen.root).build();
        window.present();
        pump_until(|| window.is_mapped(), Duration::from_secs(5));

        hooks.download_popover.popup();
        pump_until(|| hooks.download_popover_box.first_child().is_some(), Duration::from_secs(2));
        click_button_labeled(&hooks.download_popover_box, "Current chapter");

        pump_until(
            || runtime.block_on(abs_storage::repo::download_tracks::get(&pool, &server.id, "item-1", "1")).unwrap().map(|r| r.status == abs_storage::models::DownloadStatus::Complete).unwrap_or(false),
            Duration::from_secs(10),
        );
        pump_until(|| hooks.download_button.icon_name().as_deref() == Some("emblem-ok-symbolic"), Duration::from_secs(5));
        assert_eq!(hooks.download_button.icon_name().as_deref(), Some("emblem-ok-symbolic"), "the button should reflect Complete once the download finishes");

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

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
        let hooks = screen.test_hooks();
        assert_eq!(hooks.speed_label.label(), "1.0×", "should start at the default speed");
        assert!(hooks.speed_button.popover().is_some(), "the speed button should open a popover");

        click_button_labeled(&hooks.speed_popover_box, "1.5×");
        assert_eq!(controller.snapshot().unwrap().speed, 1.5, "clicking a preset should change the real controller's speed");
        assert_eq!(hooks.speed_label.label(), "1.5×");

        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. The full player's keyboard actions
    /// (ui-spec §6's full-player table): speed steps relative to the current speed, arrow-key
    /// skip against real (mock-served, seekable) audio, `c`/`t` popping their popovers, and
    /// Escape's collapse. The accelerators themselves are GTK-level (set app-wide in
    /// `application.rs`); what needs checking here is that the actions drive the same controller
    /// state their buttons do.
    pub(crate) fn run_keyboard_actions(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let collapsed = std::rc::Rc::new(std::cell::Cell::new(false));
        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), {
            let collapsed = collapsed.clone();
            move || collapsed.set(true)
        });
        let actions = &screen.actions;

        assert_eq!(controller.snapshot().unwrap().speed, 1.0);
        actions.activate_action("speed-up", None);
        assert_eq!(controller.snapshot().unwrap().speed, 1.25, "speed-up should step to the next preset");
        actions.activate_action("speed-up", None);
        assert_eq!(controller.snapshot().unwrap().speed, 1.5);
        actions.activate_action("speed-down", None);
        assert_eq!(controller.snapshot().unwrap().speed, 1.25, "speed-down should step back");
        actions.activate_action("speed-reset", None);
        assert_eq!(controller.snapshot().unwrap().speed, 1.0, "reset should return to 1×");
        // At 1× already, reset is a no-op — no redundant seek-with-rate.
        actions.activate_action("speed-reset", None);
        assert_eq!(controller.snapshot().unwrap().speed, 1.0);

        // Arrow-key skip against the real pipeline: forward clamps to the 5s item's end, back
        // lands at (near) 0.
        actions.activate_action("skip-forward", None);
        pump_until(
            || (controller.snapshot().unwrap().position_seconds - 5.0).abs() < 1.0,
            Duration::from_secs(5),
        );
        actions.activate_action("skip-back", None);
        pump_until(|| controller.snapshot().unwrap().position_seconds < 1.0, Duration::from_secs(5));

        // `c`/`t` pop the same popovers their menu buttons open — which needs a mapped toplevel,
        // per `run_chapters_sheet_lists_and_seeks`'s note.
        let window = gtk4::Window::builder().child(&screen.root).build();
        window.present();
        pump_until(|| window.is_mapped(), Duration::from_secs(5));
        let hooks = screen.test_hooks();
        actions.activate_action("chapters", None);
        pump_until(|| hooks.chapters_popover.is_visible(), Duration::from_secs(2));
        actions.activate_action("sleep-timer", None);
        pump_until(|| hooks.sleep_timer_popover.is_visible(), Duration::from_secs(2));

        actions.activate_action("collapse", None);
        assert!(collapsed.get(), "Escape's action should call on_collapse");

        window.destroy();
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

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Chaptered Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| !controller.chapters().is_empty(), Duration::from_secs(10));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
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
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some(), Duration::from_secs(10));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
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

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Clicking "Mark as finished" should
    /// pause playback and persist `is_finished = true` at the item's full duration, mirroring
    /// what actually reaching the end of a book does.
    pub(crate) fn run_mark_as_finished_button_updates_progress(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), Duration::from_secs(10));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
        let hooks = screen.test_hooks();

        hooks.mark_as_finished_button.emit_clicked();
        pump_until(|| !controller.snapshot().unwrap().is_playing, Duration::from_secs(5));
        assert!(!controller.snapshot().unwrap().is_playing, "marking finished should pause playback");

        pump_until(|| false, Duration::from_millis(300));
        let progress = runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap().unwrap();
        assert!(progress.is_finished, "should be recorded as finished");
        assert_eq!(progress.current_time_seconds, 5.0, "should be recorded at the item's full duration");
        controller.stop();
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Clicking "Reset progress" should
    /// persist position 0 (not finished) and seek the actual, currently-loaded backend back to
    /// the start too, so the effect is visible without reopening the player.
    pub(crate) fn run_reset_progress_button_resets_position_and_seeks(runtime: &tokio::runtime::Runtime) {
        let mock_server = runtime.block_on(wiremock::MockServer::start());
        runtime.block_on(mock_playable_item(&mock_server, "item-1", 5));

        let pool = runtime.block_on(crate::test_support::pool());
        let (server, account) = runtime.block_on(account_and_server(&pool, &mock_server.uri()));
        runtime.block_on(insert_synced_item(&pool, &server.id, "item-1", "Test Item"));

        let controller = crate::player::PlayerController::new(pool.clone(), crate::test_support::test_paths(), test_backend(), |_| {});
        controller.start(
            abs_core::auth::Session::new(pool.clone(), &server, &account),
            PlayRequest { item_id: "item-1".to_string(), title: "Test Book".to_string(), author: None },
            1.0,
        );
        pump_until(|| controller.snapshot().is_some_and(|s| s.is_playing), Duration::from_secs(10));
        controller.skip(2.0);
        pump_until(|| controller.snapshot().unwrap().position_seconds > 1.0, Duration::from_secs(5));

        let screen = build(pool.clone(), controller.clone(), test_download_manager(pool.clone()), || {});
        let hooks = screen.test_hooks();

        hooks.reset_progress_button.emit_clicked();
        pump_until(|| controller.snapshot().unwrap().position_seconds < 0.5, Duration::from_secs(5));
        assert!(controller.snapshot().unwrap().position_seconds < 0.5, "resetting should seek the loaded backend back to the start");

        pump_until(|| false, Duration::from_millis(300));
        let progress = runtime.block_on(abs_storage::repo::progress::get(&pool, &account.id, &server.id, "item-1")).unwrap().unwrap();
        assert!(!progress.is_finished);
        assert_eq!(progress.current_time_seconds, 0.0);
        controller.stop();
    }

    /// Finds and clicks the button with the given label anywhere inside a container, so tests can
    /// drive widgets the same way a user tapping them would. The text may sit on the button
    /// itself (the flat scope rows used to) or on a label inside it (scope rows are title +
    /// subtitle stacks now; "Next chapters" carries its title in its child box).
    fn click_button_labeled(container: &gtk4::Box, label: &str) {
        let mut found = None;
        for_each_descendant(container.upcast_ref(), &mut |widget| {
            if found.is_some() {
                return;
            }
            if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
                if button_text(button).as_deref() == Some(label) {
                    found = Some(button.clone());
                }
            }
        });
        found.unwrap_or_else(|| panic!("no button labeled {label:?} found")).emit_clicked();
    }

    /// A button's effective text: its own label, else the first label inside its child box.
    fn button_text(button: &gtk4::Button) -> Option<String> {
        if let Some(label) = button.label() {
            return Some(label.to_string());
        }
        let child = button.child()?;
        let mut text = None;
        for_each_descendant(&child, &mut |widget| {
            if text.is_none() {
                if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
                    text = Some(label.label().to_string());
                }
            }
        });
        text
    }

    /// Extracts the title label's text from a chapters-sheet row built by `build_chapter_row`.
    fn row_title(row: &gtk4::ListBoxRow) -> adw::glib::GString {
        let row_box = row.child().expect("row has a child box").downcast::<gtk4::Box>().unwrap();
        let title_label = row_box.first_child().expect("row box has a title label").downcast::<gtk4::Label>().unwrap();
        title_label.label()
    }

    /// Visits every descendant of `root` depth-first.
    fn for_each_descendant(root: &gtk4::Widget, f: &mut dyn FnMut(&gtk4::Widget)) {
        let mut child = root.first_child();
        while let Some(widget) = child {
            f(&widget);
            for_each_descendant(&widget, f);
            child = widget.next_sibling();
        }
    }

}
