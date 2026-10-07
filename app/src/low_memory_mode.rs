//! Shared, live-updating state behind Settings' "Low memory mode" switch — same shape as
//! `crate::offline_mode::LowMemoryModeState`: one value, an `Rc<RefCell<Inner>>`, permanent
//! listeners notified synchronously on the GTK main loop whenever `set()` changes it, persisted
//! fire-and-forget. What reacts to it: the cover widgets (no covers, smaller texture cache — see
//! `crate::widgets::cover_image::set_low_memory_mode`) and Home/Library (re-render so covers
//! already shown go away). The database profile is chosen once at startup (`main.rs::setup`), so
//! it follows a change on the next launch.

use std::cell::RefCell;
use std::rc::Rc;

use adw::glib;
use sqlx::SqlitePool;

type Listener = Box<dyn Fn(bool)>;
type PersistErrorHandler = Rc<dyn Fn(abs_storage::StorageError)>;

struct Inner {
    pool: SqlitePool,
    value: bool,
    listeners: Vec<Listener>,
    /// Told when persisting a change fails. The toggle has already taken effect in memory, so
    /// without this the user only finds out on next launch that it wasn't remembered.
    on_persist_error: Option<PersistErrorHandler>,
}

impl Inner {
    fn publish(&self) {
        for listener in &self.listeners {
            listener(self.value);
        }
    }
}

#[derive(Clone)]
pub struct LowMemoryModeState {
    inner: Rc<RefCell<Inner>>,
}

impl LowMemoryModeState {
    /// Starts at `false` and immediately kicks off one async `load_low_memory_mode` read, publishing
    /// the real persisted value once it lands. Callers must register their listeners
    /// synchronously, during their own `build()` (before yielding to the glib main loop) — this
    /// load is a local SQLite read with no `.await` point reached before the caller's own
    /// synchronous setup finishes, so listeners registered during `build()` are always in place in
    /// time, the same ordering `DownloadManager`/`PlayerController` listeners already rely on.
    pub fn new(pool: SqlitePool) -> Self {
        let state = Self { inner: Rc::new(RefCell::new(Inner { pool: pool.clone(), value: false, listeners: Vec::new(), on_persist_error: None })) };
        let inner_rc = state.inner.clone();
        glib::spawn_future_local(async move {
            if let Ok(value) = abs_core::settings::load_low_memory_mode(&pool).await {
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
        let (pool, on_persist_error) = {
            let mut inner = self.inner.borrow_mut();
            if inner.value == value {
                return;
            }
            inner.value = value;
            (inner.pool.clone(), inner.on_persist_error.clone())
        };
        tracing::info!(low_memory_mode = value, "low memory mode {}", if value { "turned on" } else { "turned off" });
        // `publish()` must run with no active borrow — listener callbacks call `.get()` (and
        // `apply()`/`render_from_current_data()` do too, transitively), which needs its own
        // immutable borrow, and a listener invoked while this method still held `borrow_mut()`
        // would panic with "already mutably borrowed".
        self.inner.borrow().publish();
        glib::spawn_future_local(async move {
            if let Err(err) = abs_core::settings::save_low_memory_mode(&pool, value).await {
                match on_persist_error {
                    Some(on_persist_error) => on_persist_error(err),
                    None => tracing::warn!(%err, "couldn't persist low memory mode; it won't be remembered next launch"),
                }
            }
        });
    }

    /// Sets where a failed persist is reported. The shell points this at its own toast overlay,
    /// which is on screen whichever tab the toggle was flipped from.
    pub fn set_on_persist_error(&self, handler: impl Fn(abs_storage::StorageError) + 'static) {
        self.inner.borrow_mut().on_persist_error = Some(Rc::new(handler));
    }

    /// Permanent registration, no unregister — same shape as `PlayerController::add_listener`/
    /// `DownloadManager::add_listener` (nothing in this app ever needs to stop listening once
    /// registered; screens live for the app's whole lifetime).
    pub fn add_listener(&self, listener: impl Fn(bool) + 'static) {
        self.inner.borrow_mut().listeners.push(Box::new(listener));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::test_support::pump_until;

    /// Not a `#[test]` itself — see `main.rs`'s `mod tests`. Setting the mode notifies listeners
    /// once per change, persists it, and a repeat is a no-op; a fresh state reads it back.
    pub(crate) fn run_low_memory_mode_state_notifies_and_persists(runtime: &tokio::runtime::Runtime) {
        let pool = runtime.block_on(crate::test_support::pool());
        let state = super::LowMemoryModeState::new(pool.clone());
        pump_until(|| false, Duration::from_millis(200));
        let seen: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        state.add_listener({
            let seen = seen.clone();
            move |on| seen.borrow_mut().push(on)
        });
        assert!(!state.get());

        state.set(true);
        state.set(true);
        assert!(state.get());
        assert_eq!(*seen.borrow(), vec![true], "one notification per change");
        let saved = || runtime.block_on(abs_core::settings::load_low_memory_mode(&pool)).unwrap();
        pump_until(saved, Duration::from_secs(5));
        assert!(saved(), "persisted");

        let reread = super::LowMemoryModeState::new(pool.clone());
        pump_until(|| reread.get(), Duration::from_secs(5));
        assert!(reread.get(), "a fresh state reads the stored value");
    }
}
