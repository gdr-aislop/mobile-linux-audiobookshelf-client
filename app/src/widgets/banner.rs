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
}

impl ErrorBanner {
    pub fn new() -> Self {
        let icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        let label = gtk4::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["error"])
            .build();

        let content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&icon);
        content.append(&label);

        let revealer = gtk4::Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .child(&content)
            .reveal_child(false)
            .build();

        Self { revealer, label }
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

    pub fn set_revealed(&self, revealed: bool) {
        self.revealer.set_reveal_child(revealed);
    }
}

impl Default for ErrorBanner {
    fn default() -> Self {
        Self::new()
    }
}
