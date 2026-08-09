//! [`AudioEngine`]: the sliver of `buzztalk_audio::DuplexEngine`'s surface
//! the orchestrator actually touches, reified as a trait so tests can drive
//! the whole conversation loop against a scripted fake instead of a real
//! `cpal` device.
//!
//! This crate cannot open an audio device in CI (there isn't one), and
//! `buzztalk-audio::DuplexEngine` has no test-construction path of its own
//! (it's a foreign type this crate doesn't own -- see the top-level task
//! constraints). Defining this trait here and implementing it for both the
//! real engine and a crate-local fake is what makes
//! [`crate::pipeline::Orchestrator`] testable without hardware: every
//! failure-path test in `pipeline.rs` builds an `Orchestrator` around
//! [`tests::FakeAudioEngine`] instead of [`ConversationPipeline::start`]'s
//! real `DuplexEngine::start`.
//!
//! [`ConversationPipeline::start`]: crate::ConversationPipeline::start

use buzztalk_audio::{DuplexEngine, EngineStats};
#[cfg(target_os = "macos")]
use buzztalk_audio::VoiceProcessingEngine;

/// The orchestrator's view of a full-duplex audio engine: pull capture and
/// render-reference frames, push playback, read overrun/underrun counters.
///
/// `: Send` because [`crate::pipeline::Orchestrator`] (which owns a
/// `Box<dyn AudioEngine>`) is moved onto its own thread.
pub(crate) trait AudioEngine: Send {
    /// Non-blocking pop of one capture frame, if a full one is available.
    fn try_recv_capture(&mut self) -> Option<Vec<f32>>;
    /// Non-blocking pop of one render-reference frame, if available.
    fn try_recv_render_ref(&mut self) -> Option<Vec<f32>>;
    /// Queue audio for playback; never blocks. Returns how many trailing
    /// samples were dropped because the playback ring was full.
    fn push_playback(&mut self, samples: &[f32]) -> usize;
    /// Snapshot of overrun/underrun counters.
    fn stats(&self) -> EngineStats;
    /// True once the engine's underlying streams have reported an error
    /// (device disconnected, default route changed, format renegotiated)
    /// and can no longer be trusted. Defaults to `false` for engines that
    /// cannot fail this way (the test fake, simulate mode).
    fn failed(&self) -> bool {
        false
    }
}

impl AudioEngine for DuplexEngine {
    fn try_recv_capture(&mut self) -> Option<Vec<f32>> {
        DuplexEngine::try_recv_capture(self)
    }

    fn try_recv_render_ref(&mut self) -> Option<Vec<f32>> {
        DuplexEngine::try_recv_render_ref(self)
    }

    fn push_playback(&mut self, samples: &[f32]) -> usize {
        DuplexEngine::push_playback(self, samples)
    }

    fn stats(&self) -> EngineStats {
        DuplexEngine::stats(self)
    }

    fn failed(&self) -> bool {
        DuplexEngine::stream_failed(self)
    }
}

/// Mirrors the [`DuplexEngine`] impl above, one-to-one, against
/// `buzztalk-audio`'s macOS-only `VoiceProcessingIO` engine — see
/// [`crate::pipeline::PipelineConfig::use_voice_processing`] for why a
/// caller would pick this one instead.
#[cfg(target_os = "macos")]
impl AudioEngine for VoiceProcessingEngine {
    fn try_recv_capture(&mut self) -> Option<Vec<f32>> {
        VoiceProcessingEngine::try_recv_capture(self)
    }

    fn try_recv_render_ref(&mut self) -> Option<Vec<f32>> {
        VoiceProcessingEngine::try_recv_render_ref(self)
    }

    fn push_playback(&mut self, samples: &[f32]) -> usize {
        VoiceProcessingEngine::push_playback(self, samples)
    }

    fn stats(&self) -> EngineStats {
        VoiceProcessingEngine::stats(self)
    }

    fn failed(&self) -> bool {
        VoiceProcessingEngine::stream_failed(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! A scripted [`AudioEngine`] for driving [`crate::pipeline`]'s tests
    //! without a real device. Frames are handed out one at a time as the
    //! test "offers" them -- mirroring how a real device only ever has a
    //! frame or two ready per orchestrator tick -- so `Orchestrator`'s
    //! `while let Some(frame) = engine.try_recv_*()` drain loops behave the
    //! same way they would against real hardware instead of spinning
    //! forever against an engine that always has more to give.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use buzztalk_core::FRAME_SAMPLES;

    use super::*;

    /// A fake [`AudioEngine`] plus a [`FakeAudioEngineHandle`] a test can
    /// use to script it after it's been moved into an `Orchestrator`.
    pub(crate) struct FakeAudioEngine {
        capture_available: Arc<AtomicUsize>,
        render_available: Arc<AtomicUsize>,
        push_playback_calls: Arc<AtomicUsize>,
        push_playback_samples: Arc<AtomicUsize>,
    }

    /// The test-side handle to a [`FakeAudioEngine`] already owned by an
    /// `Orchestrator`. Cloneable counters, not a lock -- safe to hold onto
    /// and poll from the test thread while the orchestrator (on its own
    /// thread, or driven directly via `tick()`) reads the same atomics.
    #[derive(Clone)]
    pub(crate) struct FakeAudioEngineHandle {
        capture_available: Arc<AtomicUsize>,
        render_available: Arc<AtomicUsize>,
        push_playback_calls: Arc<AtomicUsize>,
        push_playback_samples: Arc<AtomicUsize>,
    }

    impl FakeAudioEngine {
        /// A fresh fake engine with nothing queued, plus the handle to
        /// script it.
        pub(crate) fn new() -> (Self, FakeAudioEngineHandle) {
            let capture_available = Arc::new(AtomicUsize::new(0));
            let render_available = Arc::new(AtomicUsize::new(0));
            let push_playback_calls = Arc::new(AtomicUsize::new(0));
            let push_playback_samples = Arc::new(AtomicUsize::new(0));
            let engine = Self {
                capture_available: Arc::clone(&capture_available),
                render_available: Arc::clone(&render_available),
                push_playback_calls: Arc::clone(&push_playback_calls),
                push_playback_samples: Arc::clone(&push_playback_samples),
            };
            let handle = FakeAudioEngineHandle {
                capture_available,
                render_available,
                push_playback_calls,
                push_playback_samples,
            };
            (engine, handle)
        }
    }

    impl FakeAudioEngineHandle {
        /// Make one capture frame and one render-reference frame available
        /// for the next poll -- a healthy device tick.
        pub(crate) fn offer_frame_pair(&self) {
            self.capture_available.fetch_add(1, Ordering::Relaxed);
            self.render_available.fetch_add(1, Ordering::Relaxed);
        }

        /// Make one render-reference frame available but no capture frame --
        /// simulates a mic that's stopped delivering input (unplugged)
        /// while output keeps running.
        pub(crate) fn offer_render_only(&self) {
            self.render_available.fetch_add(1, Ordering::Relaxed);
        }

        /// How many times [`AudioEngine::push_playback`] was called.
        pub(crate) fn push_playback_calls(&self) -> usize {
            self.push_playback_calls.load(Ordering::Relaxed)
        }

        /// Total samples ever handed to [`AudioEngine::push_playback`] --
        /// proof that synthesized audio actually made it to "the speaker".
        pub(crate) fn push_playback_samples(&self) -> usize {
            self.push_playback_samples.load(Ordering::Relaxed)
        }
    }

    impl AudioEngine for FakeAudioEngine {
        fn try_recv_capture(&mut self) -> Option<Vec<f32>> {
            if self
                .capture_available
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    (n > 0).then(|| n - 1)
                })
                .is_ok()
            {
                Some(vec![0.0; FRAME_SAMPLES])
            } else {
                None
            }
        }

        fn try_recv_render_ref(&mut self) -> Option<Vec<f32>> {
            if self
                .render_available
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    (n > 0).then(|| n - 1)
                })
                .is_ok()
            {
                Some(vec![0.0; FRAME_SAMPLES])
            } else {
                None
            }
        }

        fn push_playback(&mut self, samples: &[f32]) -> usize {
            self.push_playback_calls.fetch_add(1, Ordering::Relaxed);
            self.push_playback_samples
                .fetch_add(samples.len(), Ordering::Relaxed);
            0
        }

        fn stats(&self) -> EngineStats {
            EngineStats::default()
        }
    }

    #[test]
    fn fake_engine_yields_at_most_one_offered_frame_per_poll() {
        let (mut engine, handle) = FakeAudioEngine::new();
        assert!(engine.try_recv_capture().is_none());
        handle.offer_frame_pair();
        assert!(engine.try_recv_capture().is_some());
        assert!(engine.try_recv_capture().is_none());
    }

    #[test]
    fn push_playback_is_counted() {
        let (mut engine, handle) = FakeAudioEngine::new();
        engine.push_playback(&[0.0; 10]);
        engine.push_playback(&[0.0; 5]);
        assert_eq!(handle.push_playback_calls(), 2);
        assert_eq!(handle.push_playback_samples(), 15);
    }
}
