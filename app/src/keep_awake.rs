//! The app's [`abs_core::auth::KeepAwake`]: the same GTK suspend inhibitor playback holds (see
//! `screens::main_window`'s `SuspendInhibitGuard`), taken for a token refresh. `Session` asks for
//! it from whichever thread its refresh runs on (the main loop or a Tokio worker), while GTK may
//! only be called from the main thread, so taking and releasing the inhibitor are both handed to
//! the main loop. They run in the order they were handed over, and a release that somehow ran
//! first still wins: the inhibit it would have released is then never taken.

use std::sync::{Arc, Mutex};

use adw::glib;
use gtk4::prelude::*;

/// A suspend inhibit that takes longer than this to be answered gets a warning in the log.
const SLOW_SESSION_MANAGER_CALL: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) struct GtkKeepAwake;

#[derive(Default)]
struct Hold {
    /// GTK's inhibit cookie; 0 while none is held.
    cookie: u32,
    released: bool,
}

struct Release(Arc<Mutex<Hold>>);

impl abs_core::auth::KeepAwake for GtkKeepAwake {
    fn hold(&self, reason: &'static str) -> Box<dyn Send> {
        let hold = Arc::new(Mutex::new(Hold::default()));
        let taking = hold.clone();
        glib::MainContext::default().invoke(move || {
            let mut hold = taking.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if hold.released {
                return;
            }
            let Some(app) = gtk_application() else { return };
            let started = std::time::Instant::now();
            hold.cookie = app.inhibit(None::<&gtk4::Window>, gtk4::ApplicationInhibitFlags::SUSPEND, Some(reason));
            if started.elapsed() > SLOW_SESSION_MANAGER_CALL {
                tracing::warn!(elapsed_ms = started.elapsed().as_millis() as u64, reason, "the session manager was slow to answer the suspend inhibit; the UI waited for it");
            }
        });
        Box::new(Release(hold))
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        let hold = self.0.clone();
        glib::MainContext::default().invoke(move || {
            let mut hold = hold.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            hold.released = true;
            let cookie = std::mem::take(&mut hold.cookie);
            if cookie != 0 {
                if let Some(app) = gtk_application() {
                    app.uninhibit(cookie);
                }
            }
        });
    }
}

/// The running GTK application; `None` in tests, which run without one.
fn gtk_application() -> Option<gtk4::Application> {
    gtk4::gio::Application::default().and_downcast::<gtk4::Application>()
}
