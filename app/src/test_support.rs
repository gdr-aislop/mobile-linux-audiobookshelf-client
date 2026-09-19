//! Shared helpers for GTK widget tests across `screens::*` — factored out once a second screen
//! (`home.rs`) needed the exact same fresh-database and async-pump helpers `welcome.rs` already
//! had. Only compiled under `#[cfg(test)]`.

use std::time::{Duration, Instant};

use adw::glib;
use adw::prelude::*;
use sqlx::SqlitePool;

/// A fresh, migrated, tempdir-backed SQLite database — leaked (`mem::forget`) rather than cleaned
/// up, since these are short-lived test-process databases and the tempdir would otherwise be
/// dropped (and deleted) while the pool built from it is still in use.
pub async fn pool() -> SqlitePool {
    let tmp = tempfile::tempdir().unwrap();
    let pool = abs_storage::connect_and_migrate(&tmp.path().join("db.sqlite3"))
        .await
        .unwrap();
    std::mem::forget(tmp);
    pool
}

/// A fresh, tempdir-rooted `AppPaths` for tests that construct a `PlayerController` directly
/// (cover-art caching needs somewhere to write) — same leak-the-tempdir approach as `pool()`.
pub fn test_paths() -> abs_storage::AppPaths {
    let tmp = tempfile::tempdir().unwrap();
    let paths = abs_storage::AppPaths::rooted_at(tmp.path().join("data"), tmp.path().join("cache"));
    std::mem::forget(tmp);
    paths
}

/// Drains the default `MainContext` — the same one `glib::spawn_future_local` schedules onto —
/// until `done()` returns true or `timeout` elapses. `iteration(false)` is non-blocking, so this
/// is a plain poll loop, not a nested main loop.
pub fn pump_until(done: impl Fn() -> bool, timeout: Duration) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + timeout;
    while !done() && Instant::now() < deadline {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// True if any `GtkLabel` under `root` displays exactly `text` — how scenarios assert toasts:
/// an `AdwToast`'s title lives on an internal label inside the `AdwToastOverlay`, and other
/// transient feedback (the toasts' action-button labels, stacked toasts) is easier to match
/// "anywhere under the overlay" than to chase one internal widget path through the Adwaita
/// template hierarchy.
pub fn any_label_reads(root: &gtk4::Widget, text: &str) -> bool {
    if let Some(label) = root.downcast_ref::<gtk4::Label>() {
        if label.text() == text {
            return true;
        }
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if any_label_reads(&widget, text) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}
