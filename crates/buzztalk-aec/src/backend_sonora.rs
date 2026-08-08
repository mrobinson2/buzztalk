//! Backend over the `sonora` crate (pure-Rust WebRTC APM port: AEC3 + NS +
//! AGC2 + high-pass filter).
//!
//! `sonora`'s [`AudioProcessing`] speaks deinterleaved `&[&[f32]]` frames and
//! reports [`AudioProcessingStats`] with real ERLE, so -- like the `aec3`
//! backend -- no manual energy accounting is needed.

use buzztalk_core::{AecStats, EchoCanceller, Error, Result, FRAME_SAMPLES, SAMPLE_RATE_HZ};
use sonora::config::EchoCanceller as EchoCancellerConfig;
use sonora::{AudioProcessing, Config, StreamConfig};

/// [`EchoCanceller`] backed by the `sonora` crate.
pub struct SonoraCanceller {
    apm: AudioProcessing,
    render_scratch: Vec<f32>,
    capture_scratch: Vec<f32>,
}

impl SonoraCanceller {
    /// Builds a new canceller configured for one mono 10 ms frame at
    /// [`SAMPLE_RATE_HZ`].
    pub fn new() -> Result<Self> {
        // Noise suppression and AGC2 are deliberately left disabled: this
        // crate's `EchoCanceller` trait is scoped to echo cancellation, and
        // AGC2 in particular actively re-normalizes output loudness, which
        // masks (in raw signal energy terms) exactly the reduction the AEC
        // achieved. Full-chain APM (NS/AGC) belongs in a separate stage, not
        // baked into an AEC backend.
        let config = Config {
            echo_canceller: Some(EchoCancellerConfig::default()),
            ..Default::default()
        };
        let stream_config = StreamConfig::new(SAMPLE_RATE_HZ, 1);
        debug_assert_eq!(stream_config.num_samples(), FRAME_SAMPLES);
        let apm = AudioProcessing::builder()
            .config(config)
            .capture_config(stream_config)
            .render_config(stream_config)
            .build();
        Ok(Self {
            apm,
            render_scratch: vec![0.0; FRAME_SAMPLES],
            capture_scratch: vec![0.0; FRAME_SAMPLES],
        })
    }
}

impl EchoCanceller for SonoraCanceller {
    fn process_render(&mut self, far_end: &[f32]) -> Result<()> {
        buzztalk_core::check_frame(far_end)?;
        self.render_scratch.copy_from_slice(far_end);
        let mut dest = vec![0.0f32; FRAME_SAMPLES];
        self.apm
            .process_render_f32(&[&self.render_scratch], &mut [&mut dest])
            .map_err(|e| Error::Aec(format!("sonora: render frame failed: {e}")))
    }

    fn process_capture(&mut self, near_end: &mut [f32]) -> Result<()> {
        buzztalk_core::check_frame(near_end)?;
        self.capture_scratch.copy_from_slice(near_end);
        let mut dest = vec![0.0f32; FRAME_SAMPLES];
        self.apm
            .process_capture_f32(&[&self.capture_scratch], &mut [&mut dest])
            .map_err(|e| Error::Aec(format!("sonora: capture frame failed: {e}")))?;
        near_end.copy_from_slice(&dest);
        Ok(())
    }

    fn set_stream_delay_ms(&mut self, delay_ms: u32) {
        // Errs only when the value had to be clamped to sonora's supported
        // [0, 500] ms range; processing still proceeds with the clamped
        // value, so there is nothing actionable to surface through this
        // infallible trait method.
        let _ = self.apm.set_stream_delay_ms(delay_ms as i32);
    }

    fn stats(&self) -> AecStats {
        let stats = self.apm.statistics();
        AecStats {
            erle_db: stats.echo_return_loss_enhancement.map(|v| v as f32),
            estimated_delay_ms: stats.delay_ms.and_then(|d| u32::try_from(d).ok()),
            double_talk: false,
        }
    }

    fn name(&self) -> &'static str {
        "sonora"
    }
}
