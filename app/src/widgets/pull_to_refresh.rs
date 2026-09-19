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
