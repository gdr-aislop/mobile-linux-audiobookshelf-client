//! Cheap, always-on answers to "why was the UI slow?": a watchdog that notices when the GLib main
//! loop was blocked (almost every database future is polled by it, so a blocked loop shows up in
//! `sqlx` as a "slow acquire" with idle connections), and a guard that names a synchronous UI job
//! that took long.

use std::sync::atomic::{AtomicU64, Ordering};
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

/// Every stall the watchdog has reported since the app started, added up — so a longer piece of
/// work ([`Steps`]) can say how much of its time the screen was frozen. Stalls shorter than
/// `STALL_REPORT_AFTER` aren't counted, as they aren't reported.
static MAIN_LOOP_BLOCKED_MS: AtomicU64 = AtomicU64::new(0);

/// The running total behind [`MAIN_LOOP_BLOCKED_MS`].
pub(crate) fn main_loop_blocked_total() -> Duration {
    Duration::from_millis(MAIN_LOOP_BLOCKED_MS.load(Ordering::Relaxed))
}

fn add_blocked(blocked: Duration) {
    MAIN_LOOP_BLOCKED_MS.fetch_add(blocked.as_millis() as u64, Ordering::Relaxed);
}

/// Starts the main-loop watchdog. Call once, from the main thread, after GTK is up.
pub(crate) fn start_main_loop_watchdog() {
    let due = std::rc::Rc::new(std::cell::Cell::new(Instant::now() + WATCHDOG_INTERVAL));
    glib::timeout_add_local(WATCHDOG_INTERVAL, move || {
        let now = Instant::now();
        if let Some(blocked) = stall_duration(due.get(), now, STALL_REPORT_AFTER) {
            tracing::warn!(blocked_ms = blocked.as_millis() as u64, "the main loop was blocked; database waits during this time look slow");
            add_blocked(blocked);
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

/// Times the steps of a longer piece of work — a library load: cached read, render, sync, … —
/// and logs them as one line when it finishes, with how much of that time the main loop was
/// frozen. Steps are timed one after another with [`Steps::step`]; one timed somewhere else (in a
/// background task) is added with [`Steps::record`].
pub(crate) struct Steps {
    what: &'static str,
    trigger: &'static str,
    started: Instant,
    blocked_at_start: Duration,
    open: Option<(&'static str, Instant)>,
    done: Vec<(&'static str, Duration)>,
}

impl Steps {
    pub(crate) fn new(what: &'static str, trigger: &'static str) -> Self {
        Self { what, trigger, started: Instant::now(), blocked_at_start: main_loop_blocked_total(), open: None, done: Vec::new() }
    }

    /// Ends the step in progress, if any, and starts `name`.
    pub(crate) fn step(&mut self, name: &'static str) {
        self.close();
        self.open = Some((name, Instant::now()));
    }

    /// Ends the step in progress without starting another (e.g. while waiting on a background
    /// task whose own steps are then [`Self::record`]ed).
    pub(crate) fn close(&mut self) {
        if let Some((name, started)) = self.open.take() {
            self.done.push((name, started.elapsed()));
        }
    }

    /// Adds a step that was timed elsewhere.
    pub(crate) fn record(&mut self, name: &'static str, took: Duration) {
        self.close();
        self.done.push((name, took));
    }

    /// Logs the line: `<what> finished trigger=… items=… total_ms=… <step>_ms=… …
    /// main_loop_blocked_ms=… outcome=…`.
    pub(crate) fn finish(mut self, outcome: &str, items: Option<usize>) {
        self.close();
        let blocked = main_loop_blocked_total().saturating_sub(self.blocked_at_start);
        tracing::info!("{}", summary(self.what, self.trigger, items, self.started.elapsed(), &self.done, blocked, outcome));
    }
}

/// The one line [`Steps::finish`] logs. Step names become `snake_case` `_ms` fields, in order.
pub(crate) fn summary(
    what: &str,
    trigger: &str,
    items: Option<usize>,
    total: Duration,
    steps: &[(&str, Duration)],
    blocked: Duration,
    outcome: &str,
) -> String {
    let mut line = format!("{what} finished trigger={trigger}");
    if let Some(items) = items {
        line.push_str(&format!(" items={items}"));
    }
    line.push_str(&format!(" total_ms={}", total.as_millis()));
    for (name, took) in steps {
        line.push_str(&format!(" {}_ms={}", name.replace(' ', "_"), took.as_millis()));
    }
    line.push_str(&format!(" main_loop_blocked_ms={} outcome={outcome}", blocked.as_millis()));
    line
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

    #[test]
    fn the_summary_names_every_step_in_order() {
        let line = summary(
            "library load",
            "startup",
            Some(1834),
            Duration::from_millis(9412),
            &[("cached read", Duration::from_millis(640)), ("sync", Duration::from_millis(5120)), ("covers", Duration::from_millis(1310))],
            Duration::from_millis(4630),
            "ok",
        );
        assert_eq!(
            line,
            "library load finished trigger=startup items=1834 total_ms=9412 cached_read_ms=640 sync_ms=5120 covers_ms=1310 main_loop_blocked_ms=4630 outcome=ok"
        );
    }

    #[test]
    fn the_summary_leaves_out_an_unknown_item_count() {
        let line = summary("home load", "manual", None, Duration::from_millis(5), &[], Duration::ZERO, "failed");
        assert_eq!(line, "home load finished trigger=manual total_ms=5 main_loop_blocked_ms=0 outcome=failed");
    }

    #[test]
    fn reported_stalls_add_up() {
        let before = main_loop_blocked_total();
        add_blocked(Duration::from_millis(700));
        add_blocked(Duration::from_millis(300));
        assert!(main_loop_blocked_total() - before >= Duration::from_millis(1000));
    }

    #[test]
    fn steps_are_timed_one_after_another() {
        let mut steps = Steps::new("test", "startup");
        steps.step("first");
        std::thread::sleep(Duration::from_millis(20));
        steps.step("second");
        steps.record("elsewhere", Duration::from_millis(500));
        let names: Vec<_> = steps.done.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["first", "second", "elsewhere"], "recording ends the step in progress first");
        assert!(steps.done[0].1 >= Duration::from_millis(20));
        assert_eq!(steps.done[2].1, Duration::from_millis(500));
    }
}
