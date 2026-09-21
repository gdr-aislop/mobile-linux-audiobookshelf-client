//! A one-line escape hatch for background-task failures that would otherwise only reach
//! `tracing` — see the offline-mode-toggle investigation this exists for: clicking a toggle
//! spawned queries that silently timed out, `tracing::warn!`'d, and left the user staring at a
//! button that appeared to do nothing. Anything a user just triggered (a toggle, a save) whose
//! failure can happen this way should report through here instead of warning alone.

/// Logs `err` (same as a bare `tracing::warn!` would) and shows a short-lived toast on `overlay`
/// naming what failed. `context` should read naturally before " failed" (e.g. "Saving offline
/// mode failed — try again").
pub fn report_background_error(overlay: &adw::ToastOverlay, context: &str, err: impl std::fmt::Display) {
    tracing::warn!(%err, "{context}");
    overlay.add_toast(adw::Toast::new(&format!("{context} failed — try again")));
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use adw::prelude::*;

    use crate::test_support::{any_label_reads, pump_until};

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. `AdwToastOverlay` defers unmapped
    /// toasts, so this needs a mapped window, same as every other toast-assertion scenario.
    pub(crate) fn run_report_background_error_shows_one_toast(_runtime: &tokio::runtime::Runtime) {
        let overlay = adw::ToastOverlay::new();
        let window = adw::ApplicationWindow::builder().build();
        window.set_content(Some(&overlay));
        window.present();
        pump_until(|| window.is_mapped(), Duration::from_secs(5));

        super::report_background_error(&overlay, "Saving offline mode", "pool timed out");

        pump_until(|| any_label_reads(overlay.upcast_ref(), "Saving offline mode failed — try again"), Duration::from_secs(5));
        assert!(
            any_label_reads(overlay.upcast_ref(), "Saving offline mode failed — try again"),
            "a background-error report must surface as a toast naming what failed"
        );

        window.destroy();
    }
}
