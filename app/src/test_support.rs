//! Shared helpers for GTK widget tests across `screens::*` — factored out once a second screen
//! (`home.rs`) needed the exact same fresh-database and async-pump helpers `welcome.rs` already
//! had. Only compiled under `#[cfg(test)]`.

use std::time::{Duration, Instant};

use adw::glib;
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
