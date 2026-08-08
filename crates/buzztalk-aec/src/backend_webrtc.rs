//! Backend over the `webrtc-audio-processing` crate: Rust bindings to
//! PulseAudio's repackaging of WebRTC's own C++ Audio Processing Module,
//! built from source via the `bundled` feature (meson + ninja).
//!
//! This is the implementation every pure-Rust AEC port in this crate's
//! bake-off (`aec3`, `sonora`) is trying to reproduce. `Processor` is `Send +
//! Sync` natively -- no thread-confinement wrapper needed, unlike `aec3`.

use buzztalk_core::{AecStats, EchoCanceller, Error, Result, FRAME_SAMPLES, SAMPLE_RATE_HZ};
use webrtc_audio_processing::config::EchoCanceller as EchoCancellerConfig;
use webrtc_audio_processing::{Config, Processor};

/// [`EchoCanceller`] backed by the `webrtc-audio-processing` crate (WebRTC's
/// C++ APM, `bundled` build).
pub struct WebrtcCanceller {
    processor: Processor,
    config: Config,
}

impl WebrtcCanceller {
    /// Builds a new canceller configured for one mono 10 ms frame at
    /// [`SAMPLE_RATE_HZ`].
    pub fn new() -> Result<Self> {
        let processor = Processor::new(SAMPLE_RATE_HZ)
            .map_err(|e| Error::Aec(format!("webrtc: failed to create processor: {e}")))?;
        debug_assert_eq!(processor.num_samples_per_frame(), FRAME_SAMPLES);

        // Noise suppression and gain control are deliberately left disabled
        // (both default to `None` already, set explicitly here for clarity):
        // this crate's `EchoCanceller` trait is scoped to echo cancellation,
        // and AGC in particular actively re-normalizes output loudness,
        // which masks (in raw signal energy terms) exactly the reduction the
        // AEC achieved. Full-chain APM (NS/AGC) belongs in a separate stage,
        // not baked into an AEC backend. High-pass filtering is kept on, as
        // it is for the `aec3` and `sonora` backends.
        let config = Config {
            high_pass_filter: Some(Default::default()),
            echo_canceller: Some(EchoCancellerConfig::default()),
            noise_suppression: None,
            gain_controller: None,
            ..Default::default()
        };
        processor.set_config(config);

        Ok(Self { processor, config })
    }
}

impl EchoCanceller for WebrtcCanceller {
    fn process_render(&mut self, far_end: &[f32]) -> Result<()> {
        buzztalk_core::check_frame(far_end)?;
        // `analyze_render_frame` (immutable, non-modifying) is used rather
        // than `process_render_frame` because this trait's `process_render`
        // only feeds the far-end reference and never needs a (possibly
        // modified) render output back.
        self.processor
            .analyze_render_frame([far_end])
            .map_err(|e| Error::Aec(format!("webrtc: render frame failed: {e}")))
    }

    fn process_capture(&mut self, near_end: &mut [f32]) -> Result<()> {
        buzztalk_core::check_frame(near_end)?;
        let mut channels: Vec<Vec<f32>> = vec![near_end.to_vec()];
        self.processor
            .process_capture_frame(&mut channels)
            .map_err(|e| Error::Aec(format!("webrtc: capture frame failed: {e}")))?;
        near_end.copy_from_slice(&channels[0]);
        Ok(())
    }

    fn set_stream_delay_ms(&mut self, delay_ms: u32) {
        let clamped = u16::try_from(delay_ms).unwrap_or(u16::MAX);
        self.config.echo_canceller = Some(EchoCancellerConfig::Full {
            stream_delay_ms: Some(clamped),
        });
        self.processor.set_config(self.config);
    }

    fn stats(&self) -> AecStats {
        let stats = self.processor.get_stats();
        AecStats {
            erle_db: stats.echo_return_loss_enhancement.map(|v| v as f32),
            estimated_delay_ms: stats.delay_ms,
            double_talk: false,
        }
    }

    fn name(&self) -> &'static str {
        "webrtc"
    }
}
