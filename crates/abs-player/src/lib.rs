//! A small GStreamer-backed audio playback engine behind the [`AudioBackend`] trait, so
//! `abs-core`'s playback state machine (`abs_core::playback::PlaybackState`) can be driven
//! without depending on GStreamer directly. This crate is the one place that touches real audio
//! I/O — everything else in the workspace (including `abs-core`) stays testable without a sound
//! device or a GLib main loop.

mod error;

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

pub use error::{PlayerError, Result};

/// Call once per process before constructing a [`GstBackend`]. Safe to call more than once
/// (`gstreamer::init` is idempotent).
pub fn init() -> Result<()> {
    gst::init()?;
    Ok(())
}

/// A bus event a caller should react to: end of stream (advance to the next chapter, or stop)
/// and pipeline errors (surface to the user, stop playback). Polled rather than delivered via
/// callback so this stays simple to drive from a GLib main-loop idle source or a plain timer,
/// whichever the `app` crate's event loop integration prefers.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    EndOfStream,
    Error(String),
}

/// The playback engine's public surface — implemented by [`GstBackend`] for real playback, and
/// mockable in `abs-core`'s own tests (as a trait object or a hand-rolled fake) without linking
/// GStreamer at all.
pub trait AudioBackend {
    fn load(&mut self, uri: &str) -> Result<()>;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    /// Requires the pipeline to have already reached at least `PAUSED` (i.e. `play()` or
    /// `pause()` must have been called since the last `load()`) — GStreamer cannot seek a
    /// pipeline still in `NULL`/`READY`. Callers that want duration/position available before
    /// the user presses play should call `pause()` immediately after `load()`, which is also
    /// what unblocks seeking.
    fn seek(&mut self, position: Duration) -> Result<()>;
    /// Implemented as a seek to the current position with a new rate (GStreamer has no
    /// rate-only call), so it inherits `seek`'s same "at least PAUSED" requirement.
    fn set_speed(&mut self, speed: f64) -> Result<()>;
    fn position(&self) -> Option<Duration>;
    fn duration(&self) -> Option<Duration>;
    /// Non-blocking: returns the next pending bus event, if any, without waiting.
    fn poll_event(&self) -> Option<PlayerEvent>;
}

/// A `playbin`-based [`AudioBackend`]. Uses an explicit `fakesink` audio sink when constructed
/// via [`GstBackend::new_with_sink`] (used by this crate's own tests, where there is no real
/// audio device to render to); [`GstBackend::new`] uses the system default (`autoaudiosink`).
pub struct GstBackend {
    pipeline: gst::Element,
    current_speed: f64,
}

impl GstBackend {
    pub fn new() -> Result<Self> {
        Self::build(None)
    }

    /// Build with an explicit audio sink element name (e.g. `"fakesink"`) instead of the system
    /// default — this is what makes this backend testable in a sandbox with no sound hardware.
    pub fn new_with_sink(sink_element_name: &str) -> Result<Self> {
        Self::build(Some(sink_element_name))
    }

    fn build(sink_element_name: Option<&str>) -> Result<Self> {
        let pipeline = gst::ElementFactory::make("playbin").build()?;
        if let Some(sink_name) = sink_element_name {
            let sink = gst::ElementFactory::make(sink_name).build()?;
            pipeline.set_property("audio-sink", &sink);
        }
        Ok(Self { pipeline, current_speed: 1.0 })
    }

    fn bus(&self) -> gst::Bus {
        self.pipeline.bus().expect("a playbin pipeline always has a bus")
    }
}

impl AudioBackend for GstBackend {
    fn load(&mut self, uri: &str) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.pipeline.set_property("uri", uri);
        self.current_speed = 1.0;
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Paused)?;
        Ok(())
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        let clock_time = gst::ClockTime::from_nseconds(position.as_nanos() as u64);
        let ok = self.pipeline.seek(
            self.current_speed,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            clock_time,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if ok.is_err() {
            return Err(PlayerError::SeekFailed);
        }
        Ok(())
    }

    fn set_speed(&mut self, speed: f64) -> Result<()> {
        // GStreamer has no standalone "set rate" call — a rate change is expressed as a seek to
        // the current position with a new rate. Querying position first keeps this a no-op on
        // position, matching what a caller setting "just the speed" expects.
        let position = self.position().unwrap_or_default();
        self.current_speed = speed;
        self.seek(position)
    }

    fn position(&self) -> Option<Duration> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    fn duration(&self) -> Option<Duration> {
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| Duration::from_nanos(t.nseconds()))
    }

    fn poll_event(&self) -> Option<PlayerEvent> {
        let msg = self.bus().pop()?;
        match msg.view() {
            gst::MessageView::Eos(_) => Some(PlayerEvent::EndOfStream),
            gst::MessageView::Error(e) => Some(PlayerEvent::Error(e.error().to_string())),
            _ => None,
        }
    }
}

impl Drop for GstBackend {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Write a short, silent WAV file and return its `file://` URI, so tests exercise a real
    /// GStreamer decode/demux pipeline without needing a network fetch or a bundled fixture.
    fn silent_wav_uri(tmp: &tempfile::TempDir, seconds: u32) -> String {
        let path: PathBuf = tmp.path().join("silence.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..(spec.sample_rate * seconds) {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        format!("file://{}", path.display())
    }

    fn backend() -> GstBackend {
        init().expect("gstreamer should initialize in this environment");
        GstBackend::new_with_sink("fakesink").expect("build a playbin with a fake sink")
    }

    /// Block until the pipeline leaves the ASYNC state (i.e. `PAUSED`/`PLAYING` actually took
    /// effect), so position/duration queries below aren't racing pipeline startup.
    fn wait_for_state_change(backend: &GstBackend) {
        let (_result, _current, _pending) = backend
            .pipeline
            .state(gst::ClockTime::from_seconds(5));
    }

    #[test]
    fn load_then_play_reaches_the_playing_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 2)).unwrap();
        player.play().unwrap();
        wait_for_state_change(&player);

        assert_eq!(player.pipeline.current_state(), gst::State::Playing);
    }

    #[test]
    fn pause_reaches_the_paused_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 2)).unwrap();
        player.play().unwrap();
        wait_for_state_change(&player);

        player.pause().unwrap();
        wait_for_state_change(&player);

        assert_eq!(player.pipeline.current_state(), gst::State::Paused);
    }

    #[test]
    fn duration_matches_the_generated_files_length() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 3)).unwrap();
        player.pause().unwrap(); // PAUSED is enough for GStreamer to know the duration
        wait_for_state_change(&player);

        let duration = player.duration().expect("duration should be known once PAUSED");
        // Allow slack: WAV header/container overhead can shift this by a few ms.
        assert!(
            (duration.as_secs_f64() - 3.0).abs() < 0.2,
            "expected ~3s, got {duration:?}"
        );
    }

    #[test]
    fn seek_moves_the_reported_position() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 5)).unwrap();
        player.pause().unwrap();
        wait_for_state_change(&player);

        player.seek(Duration::from_secs(2)).unwrap();
        wait_for_state_change(&player);

        let position = player.position().expect("position should be known after seeking");
        assert!(
            (position.as_secs_f64() - 2.0).abs() < 0.2,
            "expected ~2s, got {position:?}"
        );
    }

    #[test]
    fn seek_before_loading_anything_fails_rather_than_panicking() {
        let mut player = backend();
        // No `load()` call: playbin has no URI set, so a seek must error, not panic.
        let result = player.seek(Duration::from_secs(1));
        assert!(result.is_err());
    }

    #[test]
    fn set_speed_preserves_position() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 5)).unwrap();
        player.pause().unwrap();
        wait_for_state_change(&player);
        player.seek(Duration::from_secs(2)).unwrap();
        wait_for_state_change(&player);

        player.set_speed(1.5).unwrap();
        wait_for_state_change(&player);

        let position = player.position().unwrap();
        assert!(
            (position.as_secs_f64() - 2.0).abs() < 0.3,
            "changing speed should not itself jump position, got {position:?}"
        );
    }

    #[test]
    fn end_of_stream_is_reported_via_poll_event() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        // A very short clip so EOS arrives quickly.
        player.load(&silent_wav_uri(&tmp, 1)).unwrap();
        player.play().unwrap();

        let mut saw_eos = false;
        for _ in 0..100 {
            if let Some(PlayerEvent::EndOfStream) = player.poll_event() {
                saw_eos = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(saw_eos, "expected an EndOfStream event within 5s");
    }

    #[test]
    fn loading_a_new_source_resets_speed_to_normal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut player = backend();
        player.load(&silent_wav_uri(&tmp, 2)).unwrap();
        player.pause().unwrap(); // a seek (which set_speed performs) needs at least PAUSED
        wait_for_state_change(&player);
        player.set_speed(2.0).unwrap();
        assert_eq!(player.current_speed, 2.0);

        player.load(&silent_wav_uri(&tmp, 2)).unwrap();
        assert_eq!(player.current_speed, 1.0, "a fresh load should not inherit the old speed");
    }

    #[test]
    fn init_can_be_called_more_than_once() {
        init().unwrap();
        init().unwrap();
    }
}
