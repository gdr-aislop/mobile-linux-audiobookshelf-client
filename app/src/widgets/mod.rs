//! Small reusable widgets shared across screens. Most of these are compatibility shims standing
//! in for libadwaita APIs newer than this crate's `v1_2` feature ceiling (PureOS Crimson's actual
//! shipped version — see the architecture plan), each documenting exactly which real widget/
//! version it's covering for so it can be deleted the day the minimum target version moves past
//! it; `cover_image` is the exception — a plain graceful-degradation wrapper, not a version shim,
//! and `pull_to_refresh` is another: GTK4/libadwaita simply don't ship the gesture at all.

pub mod banner;
pub mod cover_image;
pub mod download_scope_menu;
pub mod item_card;
pub mod pull_to_refresh;

use std::cell::Cell;
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;

/// Swaps `window`'s content one main-loop idle tick later than requested, instead of immediately.
/// GTK's click/gesture/row-activation handling expects the widget it's currently processing a
/// press/release for to still exist in its original ancestor chain when that handling finishes;
/// calling `window.set_content(...)` synchronously *inside* that same widget's own handler
/// unparents (and can finalize) it mid-bookkeeping, which is exactly what produces GTK's "Broken
/// accounting of active state for widget" warning. Deferring by one idle callback lets the
/// originating event finish unwinding first. Every window-content swap in this app should go
/// through here rather than calling `set_content` directly, so this can't regress silently.
pub(crate) fn swap_content(window: &adw::ApplicationWindow, content: &impl glib::object::IsA<gtk4::Widget>) {
    let window = window.clone();
    let content = content.clone().upcast::<gtk4::Widget>();
    glib::idle_add_local_once(move || window.set_content(Some(&content)));
}

/// Walks `root`'s descendant tree depth-first looking for the first widget of type `T` — how
/// tests and this crate's few styled-parent cases reach into widgets GTK won't hand out handles
/// to (a `GtkMessageDialog`'s internal entry, an `AdwToast`'s title label).
pub(crate) fn find_descendant<T: glib::object::IsA<gtk4::Widget>>(root: &gtk4::Widget) -> Option<T> {
    if let Some(found) = root.downcast_ref::<T>() {
        return Some(found.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_descendant::<T>(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// Sets `GtkEditable:input-purpose` on an `AdwEntryRow` — which libadwaita doesn't expose as a
/// row property: the purpose lives on the row's internal `GtkText` editable delegate. The
/// on-screen keyboard picks its layout from this (squeekboard shows its URL layout — `/`, `:`,
/// no autocapitalization — only when the focused entry's purpose is `URL`), so fields that hold
/// addresses must set it or they get the plain text keyboard.
pub(crate) fn entry_row_input_purpose(row: &adw::EntryRow, purpose: gtk4::InputPurpose) {
    let text = row
        .delegate()
        .and_then(|delegate| delegate.downcast::<gtk4::Text>().ok())
        .expect("AdwEntryRow's editable delegate should be a GtkText");
    text.set_input_purpose(purpose);
}

/// What a *manual* sync trigger — a screen's ⋯ menu "Sync now" item, or the pull-to-refresh
/// gesture — adds to that screen's sync cycle: a toast reporting the outcome (the banner/empty
/// state stay the detailed failure surface), and the in-flight flag both of a screen's triggers
/// share so syncs can't stack (an overshot can fire repeatedly during one rubber-band, and the
/// menu is one tap away from the gesture). The automatic cycle and the empty state's "Try again"
/// pass none — they already own their feedback, and re-running them is deliberately unguarded.
///
/// Cheap to clone: both fields are reference-counted, and the clones share the one flag.
#[derive(Clone)]
pub(crate) struct ManualSync {
    toast_overlay: adw::ToastOverlay,
    in_flight: Rc<Cell<bool>>,
}

impl ManualSync {
    pub(crate) fn new(toast_overlay: &adw::ToastOverlay) -> Self {
        Self { toast_overlay: toast_overlay.clone(), in_flight: Rc::new(Cell::new(false)) }
    }

    /// Claims the manual-sync slot, or reports it taken. Called by the sync cycle when it's
    /// spawned, so both trigger sites share the guard without knowing about each other.
    pub(crate) fn claim(&self) -> bool {
        if self.in_flight.get() {
            return false;
        }
        self.in_flight.set(true);
        true
    }

    /// Releases the slot and reports the outcome as a transient toast. Called at the sync
    /// cycle's resolve step, so the toast lands only after the screen has rendered the result.
    pub(crate) fn finish(&self, ok: bool) {
        self.in_flight.set(false);
        let message = if ok { "Sync complete" } else { "Sync failed" };
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }
}
