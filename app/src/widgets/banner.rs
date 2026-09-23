//! A minimal stand-in for `AdwBanner`, which turns out to require libadwaita **1.3** — one
//! version past this crate's `v1_2` feature ceiling. Confirmed by reading libadwaita-rs's own
//! generated feature gates (`mod banner;` in its `auto/mod.rs` is behind `#[cfg(feature =
//! "v1_3")]`), not assumed; the ceiling caught this at compile time exactly as intended, rather
//! than as a runtime surprise on Crimson.
//!
//! Only the two properties this app actually needs are covered — a title and a revealed/hidden
//! state — as a plain `GtkRevealer` wrapping a small icon+label row. Delete this and switch back
//! to `adw::Banner` (same method names, deliberately) the day the app's minimum libadwaita moves
//! to 1.3+.

#[cfg(test)]
use adw::glib;
use adw::prelude::*;

#[derive(Clone)]
pub struct ErrorBanner {
    revealer: gtk4::Revealer,
    label: gtk4::Label,
    expander: gtk4::Expander,
    details_label: gtk4::Label,
    action_button: gtk4::Button,
}

impl ErrorBanner {
    pub fn new() -> Self {
        let icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        // `max_width_chars(1)` caps this label's *natural* width request regardless of `wrap` —
        // an unclamped label asks Pango for enough room to lay out the whole message on one line
        // before wrapping, and this banner sits in a plain `Box` with no horizontal scroller
        // anywhere in its ancestry (`home.rs`/`library.rs` append it straight to their root
        // `Box`), so a long server/network error message could otherwise force the whole window
        // wider than the screen — the exact failure mode `widgets::item_card`'s `title_label` doc
        // comment already documents once, for a different label.
        let label = gtk4::Label::builder()
            .wrap(true)
            .max_width_chars(1)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["error"])
            .build();

        let message_row = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .build();
        message_row.append(&icon);
        message_row.append(&label);

        // The optional trailing action (e.g. "Log in again" on an authorization failure). Hidden
        // unless `set_action_label` says otherwise; visibility is relative to the revealer's, so
        // `set_revealed` keeps owning whether the banner shows at all.
        let action_button = gtk4::Button::builder()
            .css_classes(["pill"])
            .valign(gtk4::Align::Center)
            .visible(false)
            .build();

        // Collapsed by default: raw HTTP/TLS library text isn't meant for a general audience, but
        // a self-hosted user debugging an unusual TLS/proxy setup benefits from being able to see
        // (and copy, via `selectable`) the exact underlying error. Hidden entirely — not just
        // collapsed — when there's nothing to show (see `set_details`).
        let details_label = gtk4::Label::builder()
            .wrap(true)
            .max_width_chars(1)
            .xalign(0.0)
            .selectable(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let expander = gtk4::Expander::builder()
            .label("Show details")
            .child(&details_label)
            .visible(false)
            .build();

        let content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(6)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        message_row.append(&action_button);
        content.append(&message_row);
        content.append(&expander);

        let revealer = gtk4::Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .child(&content)
            .reveal_child(false)
            .build();

        Self { revealer, label, expander, details_label, action_button }
    }

    pub fn widget(&self) -> &gtk4::Revealer {
        &self.revealer
    }

    pub fn set_title(&self, text: &str) {
        self.label.set_label(text);
    }

    #[cfg(test)]
    pub fn title(&self) -> glib::GString {
        self.label.label()
    }

    /// `Some(text)` shows the collapsed "Show details" expander with `text` inside; `None` hides
    /// the expander entirely (also collapsing it, so it doesn't reopen already-expanded next time
    /// details are shown for an unrelated error).
    pub fn set_details(&self, details: Option<&str>) {
        match details {
            Some(text) => {
                self.details_label.set_label(text);
                self.expander.set_visible(true);
            }
            None => {
                self.expander.set_expanded(false);
                self.expander.set_visible(false);
            }
        }
    }

    /// The optional trailing action button (e.g. "Log in again" on an authorization failure).
    /// `Some(label)` shows the button; wire its handler via [`Self::action_button`] once per
    /// banner — the label is pure state, the handler survives relabeling.
    pub fn set_action_label(&self, label: Option<&str>) {
        match label {
            Some(text) => {
                self.action_button.set_label(text);
                self.action_button.set_visible(true);
            }
            None => self.action_button.set_visible(false),
        }
    }

    pub fn action_button(&self) -> &gtk4::Button {
        &self.action_button
    }

    #[cfg(test)]
    pub fn action_label(&self) -> glib::GString {
        self.action_button.label().unwrap_or_default()
    }

    #[cfg(test)]
    pub fn action_visible(&self) -> bool {
        self.action_button.is_visible()
    }

    #[cfg(test)]
    pub fn details_visible(&self) -> bool {
        self.expander.is_visible()
    }

    #[cfg(test)]
    pub fn details_text(&self) -> glib::GString {
        self.details_label.label()
    }

    pub fn set_revealed(&self, revealed: bool) {
        self.revealer.set_reveal_child(revealed);
    }
}

impl Default for ErrorBanner {
    fn default() -> Self {
        Self::new()
    }
}
