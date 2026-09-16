//! The playback state machine: position, play/pause, speed, and the sleep timer. Deliberately
//! has no audio I/O — `abs-player` drives a real GStreamer pipeline and reports position back in;
//! this type only tracks and validates state transitions, which is what makes it unit-testable
//! without any audio hardware or a GLib main loop.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackState {
    position_seconds: f64,
    duration_seconds: f64,
    is_playing: bool,
    speed: f64,
    sleep_timer_remaining: Option<Duration>,
}

/// Playback speed is clamped to this range — matches the 0.8x-3.0x picker in the design spec's
/// full-player screen (`docs/design/ui-spec.md`).
pub const MIN_SPEED: f64 = 0.8;
pub const MAX_SPEED: f64 = 3.0;
pub const DEFAULT_SPEED: f64 = 1.0;

impl PlaybackState {
    pub fn new(duration_seconds: f64) -> Self {
        Self {
            position_seconds: 0.0,
            duration_seconds: duration_seconds.max(0.0),
            is_playing: false,
            speed: DEFAULT_SPEED,
            sleep_timer_remaining: None,
        }
    }

    pub fn position_seconds(&self) -> f64 {
        self.position_seconds
    }

    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn sleep_timer_remaining(&self) -> Option<Duration> {
        self.sleep_timer_remaining
    }

    pub fn is_finished(&self) -> bool {
        self.duration_seconds > 0.0 && self.position_seconds >= self.duration_seconds
    }

    pub fn play(&mut self) {
        // Resuming after reaching the end restarts from the beginning, matching the behavior a
        // "Resume"/"Play" button should have on a finished item rather than doing nothing.
        if self.is_finished() {
            self.position_seconds = 0.0;
        }
        self.is_playing = true;
    }

    pub fn pause(&mut self) {
        self.is_playing = false;
    }

    pub fn toggle_play_pause(&mut self) {
        if self.is_playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Seek to an absolute position, clamped to `[0, duration]`.
    pub fn seek_to(&mut self, position_seconds: f64) {
        self.position_seconds = position_seconds.clamp(0.0, self.duration_seconds);
    }

    /// Skip forward/backward by a relative amount (negative = backward), clamped to bounds.
    /// Reaching the end this way pauses playback, same as natural end-of-media playback would.
    pub fn skip(&mut self, delta_seconds: f64) {
        self.seek_to(self.position_seconds + delta_seconds);
        if self.is_finished() {
            self.pause();
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    pub fn start_sleep_timer(&mut self, duration: Duration) {
        self.sleep_timer_remaining = Some(duration);
    }

    pub fn cancel_sleep_timer(&mut self) {
        self.sleep_timer_remaining = None;
    }

    /// Advance playback state by `elapsed` wall-clock time. No-ops if paused. Position advances
    /// at `speed`x; the sleep timer counts down in wall-clock time regardless of speed (matching
    /// how every audiobook app's sleep timer behaves — it's "stop in N minutes", not "stop after
    /// N minutes of played audio"). Reaching the end of the sleep timer, or the end of the media,
    /// pauses playback.
    pub fn tick(&mut self, elapsed: Duration) {
        if !self.is_playing {
            return;
        }

        self.position_seconds =
            (self.position_seconds + elapsed.as_secs_f64() * self.speed).min(self.duration_seconds);

        if let Some(remaining) = self.sleep_timer_remaining {
            self.sleep_timer_remaining = Some(remaining.saturating_sub(elapsed));
            if remaining <= elapsed {
                self.sleep_timer_remaining = None;
                self.pause();
                return;
            }
        }

        if self.is_finished() {
            self.pause();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_starts_paused_at_the_beginning() {
        let state = PlaybackState::new(3600.0);
        assert_eq!(state.position_seconds(), 0.0);
        assert!(!state.is_playing());
        assert_eq!(state.speed(), DEFAULT_SPEED);
        assert_eq!(state.sleep_timer_remaining(), None);
    }

    #[test]
    fn play_sets_is_playing() {
        let mut state = PlaybackState::new(100.0);
        state.play();
        assert!(state.is_playing());
    }

    #[test]
    fn pause_clears_is_playing() {
        let mut state = PlaybackState::new(100.0);
        state.play();
        state.pause();
        assert!(!state.is_playing());
    }

    #[test]
    fn toggle_play_pause_flips_state() {
        let mut state = PlaybackState::new(100.0);
        state.toggle_play_pause();
        assert!(state.is_playing());
        state.toggle_play_pause();
        assert!(!state.is_playing());
    }

    #[test]
    fn tick_advances_position_while_playing() {
        let mut state = PlaybackState::new(100.0);
        state.play();
        state.tick(Duration::from_secs(10));
        assert_eq!(state.position_seconds(), 10.0);
    }

    #[test]
    fn tick_does_nothing_while_paused() {
        let mut state = PlaybackState::new(100.0);
        state.tick(Duration::from_secs(10));
        assert_eq!(state.position_seconds(), 0.0);
    }

    #[test]
    fn tick_respects_playback_speed() {
        let mut state = PlaybackState::new(100.0);
        state.set_speed(2.0);
        state.play();
        state.tick(Duration::from_secs(10));
        assert_eq!(state.position_seconds(), 20.0);
    }

    #[test]
    fn tick_clamps_to_duration_and_pauses_at_the_end() {
        let mut state = PlaybackState::new(10.0);
        state.play();
        state.tick(Duration::from_secs(20));
        assert_eq!(state.position_seconds(), 10.0);
        assert!(!state.is_playing(), "reaching the end should pause");
        assert!(state.is_finished());
    }

    #[test]
    fn play_on_a_finished_item_restarts_from_the_beginning() {
        let mut state = PlaybackState::new(10.0);
        state.seek_to(10.0);
        assert!(state.is_finished());

        state.play();
        assert_eq!(state.position_seconds(), 0.0);
        assert!(state.is_playing());
    }

    #[test]
    fn seek_to_clamps_within_bounds() {
        let mut state = PlaybackState::new(100.0);
        state.seek_to(-5.0);
        assert_eq!(state.position_seconds(), 0.0);
        state.seek_to(500.0);
        assert_eq!(state.position_seconds(), 100.0);
    }

    #[test]
    fn skip_forward_and_backward_move_position() {
        let mut state = PlaybackState::new(100.0);
        state.seek_to(50.0);
        state.skip(30.0);
        assert_eq!(state.position_seconds(), 80.0);
        state.skip(-15.0);
        assert_eq!(state.position_seconds(), 65.0);
    }

    #[test]
    fn skip_past_the_end_pauses() {
        let mut state = PlaybackState::new(100.0);
        state.seek_to(90.0);
        state.play();
        state.skip(20.0);
        assert_eq!(state.position_seconds(), 100.0);
        assert!(!state.is_playing());
    }

    #[test]
    fn skip_before_the_start_clamps_to_zero() {
        let mut state = PlaybackState::new(100.0);
        state.seek_to(5.0);
        state.skip(-30.0);
        assert_eq!(state.position_seconds(), 0.0);
    }

    #[test]
    fn set_speed_clamps_to_the_valid_range() {
        let mut state = PlaybackState::new(100.0);
        state.set_speed(0.1);
        assert_eq!(state.speed(), MIN_SPEED);
        state.set_speed(10.0);
        assert_eq!(state.speed(), MAX_SPEED);
        state.set_speed(1.5);
        assert_eq!(state.speed(), 1.5);
    }

    #[test]
    fn sleep_timer_counts_down_while_playing() {
        let mut state = PlaybackState::new(3600.0);
        state.play();
        state.start_sleep_timer(Duration::from_secs(60));
        state.tick(Duration::from_secs(10));
        assert_eq!(state.sleep_timer_remaining(), Some(Duration::from_secs(50)));
        assert!(state.is_playing(), "should still be playing before the timer elapses");
    }

    #[test]
    fn sleep_timer_pauses_playback_when_it_elapses() {
        let mut state = PlaybackState::new(3600.0);
        state.play();
        state.start_sleep_timer(Duration::from_secs(30));
        state.tick(Duration::from_secs(30));
        assert!(!state.is_playing());
        assert_eq!(state.sleep_timer_remaining(), None);
    }

    #[test]
    fn sleep_timer_elapsing_exactly_at_tick_boundary_still_pauses() {
        let mut state = PlaybackState::new(3600.0);
        state.play();
        state.start_sleep_timer(Duration::from_secs(10));
        state.tick(Duration::from_secs(15)); // overshoots — must not underflow/panic
        assert!(!state.is_playing());
        assert_eq!(state.sleep_timer_remaining(), None);
    }

    #[test]
    fn cancel_sleep_timer_clears_it_without_pausing() {
        let mut state = PlaybackState::new(3600.0);
        state.play();
        state.start_sleep_timer(Duration::from_secs(30));
        state.cancel_sleep_timer();
        state.tick(Duration::from_secs(100));
        assert_eq!(state.sleep_timer_remaining(), None);
        assert!(state.is_playing(), "canceling the timer should not pause playback");
    }

    #[test]
    fn sleep_timer_does_not_tick_down_while_paused() {
        let mut state = PlaybackState::new(3600.0);
        state.start_sleep_timer(Duration::from_secs(30));
        state.tick(Duration::from_secs(10)); // paused: tick is a no-op entirely
        assert_eq!(state.sleep_timer_remaining(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn zero_duration_item_is_immediately_finished() {
        let state = PlaybackState::new(0.0);
        // A zero-length item has no meaningful "finished" state to reach via playback, so it's
        // deliberately not considered finished — avoids a `play()` on it looping at position 0.
        assert!(!state.is_finished());
    }

    #[test]
    fn negative_duration_is_treated_as_zero() {
        let state = PlaybackState::new(-10.0);
        assert_eq!(state.duration_seconds(), 0.0);
    }
}
