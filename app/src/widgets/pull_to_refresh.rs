//! Pull-down-to-refresh for a screen's main `GtkScrolledWindow`. Neither GTK4 nor libadwaita
//! ships this gesture and the GNOME HIG doesn't prescribe it, so this is the hand-rolled version
//! real GNOME mobile apps (Fragments) use: the scroller's `edge-overshot` signal fires when a
//! touch drag scrolls past the top of an already-at-top scroller — kinetic scrolling rubber-bands
//! there, which *is* the pull gesture — and the screen's main scroller is the only widget that
//! can see it, since GTK's scrollable chain routes a vertical overscroll to the outermost
//! scroller (Home's horizontal shelf rows can't hijack it).
//!
//! Deliberately threshold-less: the signal carries no pull-distance payload, so a fast upward
//! fling that barely crosses the top edge also counts as a pull. That's accepted as-is for now;
//! tightening it needs calibration on real touch hardware (the same posture as the mini-player's
//! swipe-up gesture, ui-spec § "Player — mini"), not something the test sandbox can decide.

use adw::prelude::*;

/// Arms `on_refresh` to fire whenever the user pulls `scroller` past its top edge. The scroller
/// must be the screen's main vertical one — attaching it to a horizontal shelf would fire on
/// sideways overscrolls (the signal reports the edge, which is how `Top` is filtered here).
pub fn attach(scroller: &gtk4::ScrolledWindow, on_refresh: impl Fn() + 'static) {
    scroller.connect_edge_overshot(move |_, position| {
        if position == gtk4::PositionType::Top {
            on_refresh();
        }
    });
}

/// Reveal-and-spin feedback for a manual sync (pull-to-refresh or "Sync now"): a thin bar with a
/// spinner and "Syncing…" label, shown the instant the trigger fires and hidden when it resolves.
/// Not finger-tracked — see the module doc above for why a continuous drag-distance signal isn't
/// available at this crate's v4_8/v1_2 ceiling. Driven by `crate::widgets::ManualSync`, the one
/// place that already knows when a manual sync starts and finishes.
#[derive(Clone)]
pub(crate) struct PullIndicator {
    revealer: gtk4::Revealer,
    spinner: gtk4::Spinner,
}

impl PullIndicator {
    /// Builds the widget to insert into the screen's layout (same slot as the error banner —
    /// directly above the scroller) plus the handle that drives it.
    pub(crate) fn build() -> (gtk4::Revealer, Self) {
        let spinner = gtk4::Spinner::builder().spinning(false).build();
        let content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk4::Align::Center)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        content.append(&spinner);
        content.append(&gtk4::Label::new(Some("Syncing…")));
        let revealer = gtk4::Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .child(&content)
            .reveal_child(false)
            .build();
        (revealer.clone(), Self { revealer, spinner })
    }

    pub(crate) fn spinner(&self) -> &gtk4::Spinner {
        &self.spinner
    }

    pub(crate) fn show(&self) {
        self.spinner.set_spinning(true);
        self.revealer.set_reveal_child(true);
    }

    pub(crate) fn hide(&self) {
        self.revealer.set_reveal_child(false);
        self.spinner.set_spinning(false);
    }
}
