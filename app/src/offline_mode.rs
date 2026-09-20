//! Shared, live-updating state behind the Home/Library "offline mode" toggle — per
//! `docs/design/ui-spec.md`, "state shared with the equivalent toggle on Library browse, not a
//! per-screen setting". Built once in `main_window.rs` and cloned into `screens::home::build`/
//! `screens::library::build`, same shape as `crate::downloads::DownloadManager`/
//! `crate::player::PlayerController`: an `Rc<RefCell<Inner>>` with a permanent
//! `listeners: Vec<Box<dyn Fn(bool)>>`, notified synchronously on the GTK main loop whenever
//! `set()` changes the value.
//!
//! Before this existed, each screen kept its own private `Rc<Cell<bool>>`, independently loaded
//! once at build time and never told about the other screen's toggle — so both screens were built
//! once, up front, and kept alive together for the app's whole run (see `main_window.rs`), and
//! toggling one never reached the other until the next app launch. This type exists specifically
//! to fix that: one value, one source of truth, every listener notified on every change regardless
//! of which screen caused it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::glib;
use sqlx::SqlitePool;

type Listener = Box<dyn Fn(bool)>;

struct Inner {
    pool: SqlitePool,
    value: bool,
    listeners: Vec<Listener>,
}

impl Inner {
    fn publish(&self) {
        for listener in &self.listeners {
            listener(self.value);
        }
    }
}

#[derive(Clone)]
pub struct OfflineModeState {
    inner: Rc<RefCell<Inner>>,
}

impl OfflineModeState {
    /// Starts at `false` and immediately kicks off one async `load_offline_mode` read, publishing
    /// the real persisted value once it lands. Callers must register their listeners
    /// synchronously, during their own `build()` (before yielding to the glib main loop) — this
    /// load is a local SQLite read with no `.await` point reached before the caller's own
    /// synchronous setup finishes, so listeners registered during `build()` are always in place in
    /// time, the same ordering `DownloadManager`/`PlayerController` listeners already rely on.
    pub fn new(pool: SqlitePool) -> Self {
        let state = Self { inner: Rc::new(RefCell::new(Inner { pool: pool.clone(), value: false, listeners: Vec::new() })) };
        let inner_rc = state.inner.clone();
        glib::spawn_future_local(async move {
            if let Ok(value) = abs_core::settings::load_offline_mode(&pool).await {
                inner_rc.borrow_mut().value = value;
                // See `set()`'s comment: `publish()` must run with no active borrow.
                inner_rc.borrow().publish();
            }
        });
        state
    }

    pub fn get(&self) -> bool {
        self.inner.borrow().value
    }

    /// Updates in-memory state, notifies every listener synchronously (including the caller's
    /// own — a screen's toggle handler and its "react to the other screen changing it" path are
    /// the same listener, not two separate code paths), then persists the change fire-and-forget.
    /// No-ops entirely (no publish, no write) if the value is unchanged — this, together with the
    /// widget-sync guard each screen's listener applies, is what keeps a listener-driven
    /// `toggle.set_active(...)` from re-triggering `connect_toggled` into an infinite loop.
    pub fn set(&self, value: bool) {
        let pool = {
            let mut inner = self.inner.borrow_mut();
            if inner.value == value {
                return;
            }
            inner.value = value;
            inner.pool.clone()
        };
        // `publish()` must run with no active borrow — listener callbacks call `.get()` (and
        // `apply()`/`render_from_current_data()` do too, transitively), which needs its own
        // immutable borrow, and a listener invoked while this method still held `borrow_mut()`
        // would panic with "already mutably borrowed".
        self.inner.borrow().publish();
        glib::spawn_future_local(async move {
            if let Err(err) = abs_core::settings::save_offline_mode(&pool, value).await {
                tracing::warn!(%err, "couldn't persist offline mode; it won't be remembered next launch");
            }
        });
    }

    /// Permanent registration, no unregister — same shape as `PlayerController::add_listener`/
    /// `DownloadManager::add_listener` (nothing in this app ever needs to stop listening once
    /// registered; screens live for the app's whole lifetime).
    pub fn add_listener(&self, listener: impl Fn(bool) + 'static) {
        self.inner.borrow_mut().listeners.push(Box::new(listener));
    }
}
