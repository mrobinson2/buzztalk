//! Shared voicing/energy front-end.
//!
//! Both detectors in this crate need the same two cheap per-frame features:
//! frame energy relative to an adaptively-tracked noise floor, and
//! zero-crossing rate. Deliberately not a neural VAD -- this crate is the
//! deterministic baseline the whole gating story (ERLE gate, route gate,
//! convergence guard, spectral-variation check) is tested against, and a
//! model would make those tests non-deterministic and hardware/training-data
//! dependent.

use buzztalk_core::{check_frame, Result};

/// Tunable parameters for [`VoicingFrontend`]'s adaptive noise floor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrontEndConfig {
    /// Noise-floor estimate before any frames have been seen, in dBFS.
    pub initial_floor_dbfs: f32,
    /// How fast the floor is allowed to *rise* per frame, in dB.
    ///
    /// Kept small on purpose: the floor is a minimum-follower. It should
    /// track ambient noise between utterances, not get dragged upward by a
    /// loud, sustained voice. Falls happen immediately (see
    /// [`VoicingFrontend::process`]); this only bounds the rise.
    pub floor_rise_db_per_frame: f32,
    /// Lower clamp for both the energy estimate and the floor, in dBFS. Pure
    /// digital silence has no defined dB value (`log10(0)`); this keeps the
    /// math finite.
    pub silence_floor_dbfs: f32,
}

impl Default for FrontEndConfig {
    fn default() -> Self {
        Self {
            initial_floor_dbfs: -50.0,
            // 0.05 dB/frame == 5 dB/s. Real noise floors drift over seconds,
            // not fractions of a second; a faster rise would let a single
            // sustained utterance erode its own SNR margin mid-word.
            floor_rise_db_per_frame: 0.05,
            silence_floor_dbfs: -90.0,
        }
    }
}

/// Per-frame features computed by [`VoicingFrontend`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameFeatures {
    /// Frame energy in dBFS (`10 * log10(mean(sample^2))`, clamped at the
    /// configured silence floor).
    pub energy_dbfs: f32,
    /// Current adaptive noise-floor estimate, in dBFS.
    pub noise_floor_dbfs: f32,
    /// `energy_dbfs - noise_floor_dbfs`. The quantity both detectors threshold
    /// on, rather than raw energy, so a loud room and a quiet room are judged
    /// the same way.
    pub snr_db: f32,
    /// Zero-crossing rate: sign changes per sample, in `[0, 1]`.
    pub zcr: f32,
}

/// Pure-Rust, model-free energy/voicing front-end shared by both detectors.
///
/// Each detector owns its own instance -- the noise-floor estimate and its
/// thresholding are independent per detector, since the endpoint and
/// barge-in detectors are tuned to different sensitivities.
#[derive(Debug, Clone)]
pub struct VoicingFrontend {
    config: FrontEndConfig,
    floor_dbfs: f32,
}

impl VoicingFrontend {
    /// Build a front-end with the given configuration.
    pub fn new(config: FrontEndConfig) -> Self {
        let floor_dbfs = config.initial_floor_dbfs;
        Self { config, floor_dbfs }
    }

    /// Process one frame, updating the internal noise-floor estimate and
    /// returning the computed features.
    ///
    /// # Errors
    ///
    /// Returns [`buzztalk_core::Error::FrameLength`] if `frame` is not
    /// exactly [`buzztalk_core::FRAME_SAMPLES`] long.
    pub fn process(&mut self, frame: &[f32]) -> Result<FrameFeatures> {
        check_frame(frame)?;

        let energy_dbfs = frame_energy_dbfs(frame, self.config.silence_floor_dbfs);

        // Minimum-follower: fall immediately to a quieter frame (that's the
        // best estimate of ambient noise we've seen so far), rise slowly
        // otherwise, and never rise above the current frame's own energy.
        if energy_dbfs < self.floor_dbfs {
            self.floor_dbfs = energy_dbfs;
        } else {
            self.floor_dbfs =
                (self.floor_dbfs + self.config.floor_rise_db_per_frame).min(energy_dbfs);
        }
        self.floor_dbfs = self.floor_dbfs.max(self.config.silence_floor_dbfs);

        let snr_db = energy_dbfs - self.floor_dbfs;
        let zcr = zero_crossing_rate(frame);

        Ok(FrameFeatures {
            energy_dbfs,
            noise_floor_dbfs: self.floor_dbfs,
            snr_db,
            zcr,
        })
    }

    /// Reset the noise-floor estimate to its initial value.
    pub fn reset(&mut self) {
        self.floor_dbfs = self.config.initial_floor_dbfs;
    }
}

fn frame_energy_dbfs(frame: &[f32], floor: f32) -> f32 {
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    let mean_sq = sum_sq / frame.len() as f32;
    if mean_sq <= 0.0 {
        floor
    } else {
        (10.0 * mean_sq.log10()).max(floor)
    }
}

fn zero_crossing_rate(frame: &[f32]) -> f32 {
    if frame.len() < 2 {
        return 0.0;
    }
    let crossings = frame
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count();
    crossings as f32 / (frame.len() - 1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_core::FRAME_SAMPLES;

    #[test]
    fn silence_has_zero_energy_and_zero_crossings() {
        let mut fe = VoicingFrontend::new(FrontEndConfig::default());
        let frame = vec![0.0_f32; FRAME_SAMPLES];
        let features = fe.process(&frame).unwrap();
        assert_eq!(features.zcr, 0.0);
        assert_eq!(features.energy_dbfs, fe_silence_floor());
    }

    fn fe_silence_floor() -> f32 {
        FrontEndConfig::default().silence_floor_dbfs
    }

    #[test]
    fn floor_falls_fast_and_rises_slowly() {
        let mut fe = VoicingFrontend::new(FrontEndConfig::default());
        let loud = vec![0.5_f32; FRAME_SAMPLES];
        let quiet = vec![0.0_f32; FRAME_SAMPLES];

        // Loud frame from the (higher) initial floor should immediately
        // raise the *energy* reading but only nudge the floor up slightly.
        let after_loud = fe.process(&loud).unwrap();
        assert!(after_loud.noise_floor_dbfs < after_loud.energy_dbfs);

        // A quiet frame right after should snap the floor straight down.
        let after_quiet = fe.process(&quiet).unwrap();
        assert_eq!(after_quiet.noise_floor_dbfs, after_quiet.energy_dbfs);
    }

    #[test]
    fn wrong_length_frame_is_rejected() {
        let mut fe = VoicingFrontend::new(FrontEndConfig::default());
        assert!(fe.process(&[0.0; 10]).is_err());
    }
}
