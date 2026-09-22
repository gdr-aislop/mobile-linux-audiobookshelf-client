//! A tappable library-item card: cover art, title, subtitle, wrapped in a flat button that opens
//! the Item Detail screen (`docs/design/ui-spec.md`'s "tap -> Item detail -> Play" flow). Shared
//! between `screens::home` (shelf cards) and `screens::library` (grid tiles) — extracted from
//! `home.rs`'s original `cover_card` once a second screen needed the exact same widget, same
//! reasoning as `test_support.rs`'s own extraction history.

use adw::prelude::*;
use std::rc::Rc;

use abs_storage::models::Item;

use crate::player::PlayRequest;
use crate::widgets::cover_image::CoverImage;

/// `size` is both the cover's width/height and the card's fixed width. `is_downloaded` shows a
/// small corner badge on the cover when this item has at least one completed track
/// (`abs_core::download_tracks::downloaded_item_ids`) — read-only here; the actual download
/// button/scope picker lives on the Item Detail (and Player) screen. `on_open` reports the tapped
/// item's `PlayRequest` up to the caller, which opens Item Detail for it — this card never starts
/// playback itself.
pub fn build(size: i32, item: &Item, subtitle: &str, on_open: &Rc<dyn Fn(PlayRequest)>, wrap_title: bool, is_downloaded: bool) -> gtk4::Widget {
    // Every widget in this card is explicitly `hexpand(false)` — a `GtkBox`'s own hexpand is
    // computed from its children unless overridden, so a single stray `true` here would propagate
    // all the way up to the wrapping `GtkButton` and stretch a single-item shelf/row's card full
    // width (this bit a real render once; see `home.rs`'s git history before this extraction).
    let card = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).width_request(size).hexpand(false).spacing(6).build();

    let cover = CoverImage::new(size);
    cover.widget().set_hexpand(false);
    cover.set_path(item.cover_cache_path.as_deref().map(std::path::Path::new));

    // The badge sits in an `Overlay` rather than being part of `CoverImage` itself — `CoverImage`
    // is shared with the Player screen's large cover, which has no such badge, so keeping this
    // card-only concern here avoids threading an unused parameter through every other caller.
    let cover_overlay = gtk4::Overlay::builder().child(cover.widget()).hexpand(false).build();
    let downloaded_badge = gtk4::Image::builder()
        .icon_name("folder-download-symbolic")
        .css_classes(["osd"])
        .halign(gtk4::Align::End)
        .valign(gtk4::Align::Start)
        .margin_top(4)
        .margin_end(4)
        .visible(is_downloaded)
        .build();
    cover_overlay.add_overlay(&downloaded_badge);

    // `max_width_chars(1)` matters regardless of `wrap_title` — it caps the label's own *natural
    // width request*, not just how it renders. An unclamped label asks for enough room to lay out
    // its full, un-ellipsized/un-wrapped text, and a `GtkBox`'s own natural width is the max of its
    // children's, so that request overrides this card's `width_request` (which only sets a
    // *minimum*, not a cap). Inside `home.rs`'s plain, non-homogeneous shelf row this went
    // unnoticed since each card just renders at its own natural size; inside this widget's other
    // consumer (`screens::library`'s homogeneous `GtkFlowBox`, which sizes every cell to the single
    // widest child's natural size), one long title was enough to force the *entire grid* into a
    // single column — confirmed live. `max_width_chars` caps the natural size Pango asks for,
    // letting `ellipsize`/`wrap` do their normal job at whatever width the parent allocates.
    //
    // `wrap_title` picks between two real, different needs of this card's two callers:
    // - `false` (Home's shelf cards): single-line + ellipsize, deliberately not `wrap` — a wrapped
    //   label combined with `ellipsize` asks Pango for a natural height that doesn't actually
    //   reserve room for the wrapped line count (confirmed live). Fixed single-line height keeps
    //   every card in a shelf row the same height regardless of title length.
    // - `true` (Library's grid tiles): full title, wrapped across as many lines as needed, no
    //   ellipsize — the whole point being nothing gets cut off. Trade-off, left as-is rather than
    //   chased further: `library.rs`'s grid is `homogeneous(true)`, so once titles wrap to
    //   different line counts, every cell in the grid grows to match the tallest currently-visible
    //   title, leaving uneven whitespace under short-titled items. Not worth a masonry/
    //   variable-height layout for this pass.
    let title_label = gtk4::Label::builder()
        .label(&item.title)
        .max_width_chars(1)
        .xalign(0.0)
        .hexpand(false)
        .css_classes(["heading"])
        .build();
    if wrap_title {
        title_label.set_wrap(true);
        title_label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    } else {
        title_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    }

    let meta = gtk4::Label::builder()
        .label(subtitle)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .xalign(0.0)
        .hexpand(false)
        .css_classes(["caption", "dim-label"])
        .build();

    card.append(&cover_overlay);
    card.append(&title_label);
    card.append(&meta);

    // `halign(Center)` on the button matters once this card sits in a container that can allocate
    // it more width than its natural size — a `GtkButton` (and the `GtkBox` inside it) defaults
    // `halign` to `Fill`, so without this a card placed in a `GtkFlowBox` cell wider than `size`
    // (e.g. a narrow window where only one column fits) stretches into a wide rectangle instead of
    // staying a fixed `size`-wide card. `hexpand(false)` alone doesn't prevent this — it only stops
    // the widget from *requesting* extra space, not from filling space a parent already gave it.
    // Home's shelf rows never hit this because a plain `GtkBox` row only ever allocates each child
    // its own natural size; `GtkFlowBox` cells are the first place this card's size is stretched by
    // its container. Confirmed live: without this, a single-column Library grid rendered every
    // card's cover as a wide, squashed rectangle instead of a `size`x`size` square.
    let button = gtk4::Button::builder().css_classes(["flat"]).hexpand(false).halign(gtk4::Align::Center).child(&card).build();
    let request = PlayRequest { item_id: item.id.clone(), title: item.title.clone(), author: item.author.clone() };
    let on_open = on_open.clone();
    button.connect_clicked(move |_| on_open(request.clone()));

    button.upcast()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    fn fixture_item() -> Item {
        Item {
            id: "item-1".to_string(),
            server_id: "server-1".to_string(),
            library_id: "lib-1".to_string(),
            title: "Project Hail Mary".to_string(),
            author: Some("Andy Weir".to_string()),
            narrator: None,
            description: None,
            cover_cache_path: None,
            duration_seconds: 3600.0,
            added_at: chrono::Utc::now(),
            synced_at: chrono::Utc::now(),
            series_name: None,
            genres_json: "[]".to_string(),
        }
    }

    fn title_label_of(widget: &gtk4::Widget) -> gtk4::Label {
        let button = widget.clone().downcast::<gtk4::Button>().expect("item_card::build returns a GtkButton");
        let card_box = button.child().and_then(|w| w.downcast::<gtk4::Box>().ok()).expect("button wraps the card box");
        card_box
            .first_child()
            .and_then(|cover_overlay| cover_overlay.next_sibling())
            .and_then(|w| w.downcast::<gtk4::Label>().ok())
            .expect("card's second child is the title label")
    }

    fn downloaded_badge_of(widget: &gtk4::Widget) -> gtk4::Image {
        let button = widget.clone().downcast::<gtk4::Button>().expect("item_card::build returns a GtkButton");
        let card_box = button.child().and_then(|w| w.downcast::<gtk4::Box>().ok()).expect("button wraps the card box");
        let cover_overlay = card_box.first_child().and_then(|w| w.downcast::<gtk4::Overlay>().ok()).expect("card's first child is the cover overlay");
        // The badge is the overlay child, not `Overlay::child()` (the cover itself) — walk the
        // overlay's own children rather than assume ordering beyond "added after the cover".
        let mut child = cover_overlay.first_child();
        while let Some(widget) = child {
            if let Ok(image) = widget.clone().downcast::<gtk4::Image>() {
                return image;
            }
            child = widget.next_sibling();
        }
        panic!("cover overlay should contain the downloaded-badge image");
    }

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run() {
        let item = fixture_item();

        let received: Rc<RefCell<Vec<PlayRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let on_open: Rc<dyn Fn(PlayRequest)> = {
            let received = received.clone();
            Rc::new(move |request: PlayRequest| received.borrow_mut().push(request))
        };

        let widget = build(132, &item, "Andy Weir · 1.0h", &on_open, false, false);
        let button = widget.clone().downcast::<gtk4::Button>().expect("item_card::build returns a GtkButton");
        button.emit_clicked();

        assert_eq!(received.borrow().len(), 1, "clicking the card should invoke on_open exactly once");
        assert_eq!(received.borrow()[0].item_id, "item-1");
        assert_eq!(received.borrow()[0].title, "Project Hail Mary");

        let title_label = title_label_of(&widget);
        assert!(!title_label.wraps(), "wrap_title: false should keep the single-line ellipsized title");
        assert_eq!(title_label.ellipsize(), gtk4::pango::EllipsizeMode::End);

        assert!(!downloaded_badge_of(&widget).is_visible(), "is_downloaded: false should hide the badge");
    }

    /// The downloaded badge is purely read-only — it just reflects `is_downloaded`.
    pub(crate) fn run_downloaded_badge_shows_only_when_downloaded() {
        let item = fixture_item();
        let on_open: Rc<dyn Fn(PlayRequest)> = Rc::new(|_| {});

        let widget = build(132, &item, "Andy Weir · 1.0h", &on_open, false, true);
        assert!(downloaded_badge_of(&widget).is_visible(), "is_downloaded: true should show the badge");
    }

    /// `wrap_title: true` (Library's grid tiles) should show the full title across multiple lines
    /// instead of cutting it off — the opposite trade-off from Home's shelf cards.
    pub(crate) fn run_wrap_title_shows_the_full_title_without_ellipsizing() {
        let item = fixture_item();
        let on_open: Rc<dyn Fn(PlayRequest)> = Rc::new(|_| {});

        let widget = build(108, &item, "Andy Weir · 1.0h", &on_open, true, false);
        let title_label = title_label_of(&widget);

        assert!(title_label.wraps(), "wrap_title: true should wrap instead of ellipsizing");
        assert_eq!(title_label.ellipsize(), gtk4::pango::EllipsizeMode::None, "no ellipsize when wrapping — the whole point is nothing gets cut off");
        assert_eq!(title_label.text(), "Project Hail Mary", "the full title text should still be set, just wrapped rather than truncated");
    }
}
