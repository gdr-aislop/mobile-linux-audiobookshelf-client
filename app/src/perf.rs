//! Cheap, always-on answers to "why was the UI slow?": a watchdog that notices when the GLib main
//! loop was blocked (almost every database future is polled by it, so a blocked loop shows up in
//! `sqlx` as a "slow acquire" with idle connections), and a guard that names a synchronous UI job
//! that took long.

use std::time::{Duration, Instant};

use adw::glib;

/// How often the watchdog's timer is meant to fire.
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(100);
/// A timer firing this much later than due means the loop was blocked in between.
const STALL_REPORT_AFTER: Duration = Duration::from_millis(500);
/// A synchronous UI job slower than this is logged by name.
const SLOW_JOB_AFTER: Duration = Duration::from_millis(150);

/// How long the main loop was blocked, given when the timer was due and when it actually fired;
/// `None` when it was on time (or close enough).
pub(crate) fn stall_duration(due: Instant, fired: Instant, report_after: Duration) -> Option<Duration> {
    let late = fired.saturating_duration_since(due);
    (late >= report_after).then_some(late)
}

/// Starts the main-loop watchdog. Call once, from the main thread, after GTK is up.
pub(crate) fn start_main_loop_watchdog() {
    let due = std::rc::Rc::new(std::cell::Cell::new(Instant::now() + WATCHDOG_INTERVAL));
    glib::timeout_add_local(WATCHDOG_INTERVAL, move || {
        let now = Instant::now();
        if let Some(blocked) = stall_duration(due.get(), now, STALL_REPORT_AFTER) {
            tracing::warn!(blocked_ms = blocked.as_millis() as u64, "the main loop was blocked; database waits during this time look slow");
        }
        due.set(now + WATCHDOG_INTERVAL);
        glib::ControlFlow::Continue
    });
}

/// Logs a warning on drop if the job it was created for took long. Hold it for the duration of a
/// synchronous UI job: `let _slow = perf::SlowJob::new("library render");`.
pub(crate) struct SlowJob {
    what: &'static str,
    started: Instant,
}

impl SlowJob {
    pub(crate) fn new(what: &'static str) -> Self {
        Self { what, started: Instant::now() }
    }
}

impl Drop for SlowJob {
    fn drop(&mut self) {
        let took = self.started.elapsed();
        if took >= SLOW_JOB_AFTER {
            tracing::warn!(job = self.what, took_ms = took.as_millis() as u64, "a UI job held up the main loop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_big_lateness_is_a_stall() {
        let due = Instant::now();
        let report = Duration::from_millis(500);
        assert_eq!(stall_duration(due, due + Duration::from_millis(120), report), None);
        assert_eq!(stall_duration(due, due + Duration::from_millis(499), report), None);
        assert_eq!(stall_duration(due, due + Duration::from_millis(500), report), Some(Duration::from_millis(500)));
        assert_eq!(stall_duration(due + Duration::from_secs(1), due, report), None, "a timer that fired early was not late");
    }
}
