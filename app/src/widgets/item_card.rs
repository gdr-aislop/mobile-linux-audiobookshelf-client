//! A tappable library-item card: cover art, title, subtitle, wrapped in a flat button that starts
//! playback. Shared between `screens::home` (shelf cards) and `screens::library` (grid tiles) —
//! extracted from `home.rs`'s original `cover_card` once a second screen needed the exact same
//! widget, same reasoning as `test_support.rs`'s own extraction history.

use adw::prelude::*;
use std::rc::Rc;

use abs_storage::models::Item;

use crate::player::PlayRequest;
use crate::widgets::cover_image::CoverImage;

/// `size` is both the cover's width/height and the card's fixed width. There's no Item Detail
/// screen yet, so tapping directly starts playback rather than the ui-spec's real
/// "tap -> Item detail -> Play" flow (same "skip screens not yet built" scoping already used for
/// Library/Downloads/Settings' stub tabs).
pub fn build(size: i32, item: &Item, subtitle: &str, on_play: &Rc<dyn Fn(PlayRequest)>) -> gtk4::Widget {
    // Every widget in this card is explicitly `hexpand(false)` — a `GtkBox`'s own hexpand is
    // computed from its children unless overridden, so a single stray `true` here would propagate
    // all the way up to the wrapping `GtkButton` and stretch a single-item shelf/row's card full
    // width (this bit a real render once; see `home.rs`'s git history before this extraction).
    let card = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).width_request(size).hexpand(false).spacing(6).build();

    let cover = CoverImage::new(size);
    cover.widget().set_hexpand(false);
    cover.set_path(item.cover_cache_path.as_deref().map(std::path::Path::new));

    // Single-line + ellipsize for both labels, deliberately not `wrap`: a wrapped label combined
    // with `ellipsize` asks Pango for a natural height that doesn't actually reserve room for the
    // wrapped line count (confirmed live). Fixed single-line height per label keeps every card the
    // same height regardless of title/author length.
    //
    // `max_width_chars(1)` matters as much as `ellipsize` does here: `ellipsize` alone only
    // affects what's drawn, not the label's own *natural width request* — an unclamped label still
    // asks for enough room to lay out its full, un-ellipsized text, and a `GtkBox`'s own natural
    // width is the max of its children's, so that request overrides this card's `width_request`
    // (which only sets a *minimum*, not a cap). Inside `home.rs`'s plain, non-homogeneous shelf
    // row this went unnoticed since each card just renders at its own natural size; inside this
    // widget's other consumer (`screens::library`'s `GtkFlowBox`, which sizes every homogeneous
    // cell to the single widest child's natural size), one long title was enough to force the
    // *entire grid* into a single column — confirmed live. `max_width_chars` caps the natural size
    // Pango asks for, letting `ellipsize` do its normal job at whatever width the parent allocates.
    let title_label = gtk4::Label::builder()
        .label(&item.title)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .xalign(0.0)
        .hexpand(false)
        .css_classes(["heading"])
        .build();

    let meta = gtk4::Label::builder()
        .label(subtitle)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .xalign(0.0)
        .hexpand(false)
        .css_classes(["caption", "dim-label"])
        .build();

    card.append(cover.widget());
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
    let on_play = on_play.clone();
    button.connect_clicked(move |_| on_play(request.clone()));

    button.upcast()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests` for why every fast GTK-touching
    /// scenario in this binary has to run from one single entry point.
    pub(crate) fn run() {
        let item = Item {
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
        };

        let received: Rc<RefCell<Vec<PlayRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let on_play: Rc<dyn Fn(PlayRequest)> = {
            let received = received.clone();
            Rc::new(move |request: PlayRequest| received.borrow_mut().push(request))
        };

        let widget = build(132, &item, "Andy Weir · 1.0h", &on_play);
        let button = widget.downcast::<gtk4::Button>().expect("item_card::build returns a GtkButton");
        button.emit_clicked();

        assert_eq!(received.borrow().len(), 1, "clicking the card should invoke on_play exactly once");
        assert_eq!(received.borrow()[0].item_id, "item-1");
        assert_eq!(received.borrow()[0].title, "Project Hail Mary");
    }
}
