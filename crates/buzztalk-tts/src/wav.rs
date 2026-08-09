//! Reference voice WAV loading and linear resampling.
//!
//! Ported from `buzz-voice`'s `src/pocket.rs` (`load_voice_style`, using
//! `sherpa_onnx::Wave`) and `src/pocket_april.rs` (`voice_embeddings`, using
//! `sherpa_onnx::LinearResampler`) — Apache-2.0, see the crate-level
//! attribution in `lib.rs`.
//!
//! This module is also *why* `sherpa-onnx` earns its keep as a real
//! dependency rather than an inert Cargo.toml line: calling into its Rust
//! API here is what makes Cargo actually pull its statically-linked ONNX
//! Runtime into the final linker invocation. See the comment on the
//! `sherpa-onnx` entry in `Cargo.toml` for why that matters.

use std::path::Path;

use crate::error::{Error, Result};

/// Loaded reference voice samples, mono `f32` in `[-1, 1]`, at their
/// original sample rate.
#[derive(Debug, Clone)]
pub struct VoiceStyle {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Load a reference voice WAV.
pub fn load_voice_style(path: &Path) -> Result<VoiceStyle> {
    let path_str = path.to_str().ok_or_else(|| {
        Error::Synthesis(format!("voice path is not valid UTF-8: {}", path.display()))
    })?;
    let wave = sherpa_onnx::Wave::read(path_str).ok_or_else(|| Error::Wav {
        path: path.to_path_buf(),
        reason: "sherpa-onnx could not open or decode this file".to_string(),
    })?;
    let samples = wave.samples().to_vec();
    if samples.is_empty() {
        return Err(Error::EmptyVoice(path.to_path_buf()));
    }
    Ok(VoiceStyle {
        samples,
        sample_rate: wave.sample_rate() as u32,
    })
}

/// Linearly resample `samples` from `source_rate` to `target_rate`.
pub fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Result<Vec<f32>> {
    if samples.is_empty() || source_rate == target_rate {
        return Ok(samples.to_vec());
    }
    let resampler = sherpa_onnx::LinearResampler::create(source_rate as i32, target_rate as i32)
        .ok_or_else(|| {
            Error::Synthesis(format!(
                "could not create resampler {source_rate}Hz -> {target_rate}Hz"
            ))
        })?;
    Ok(resampler.resample(samples, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the statically linked sherpa-onnx native library
    // directly (signal processing only, no model files), so they run in CI
    // without any model on disk.

    #[test]
    fn resample_identity_when_rates_match() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&samples, 24_000, 24_000).unwrap(), samples);
    }

    #[test]
    fn resample_changes_length_proportionally() {
        let samples = vec![0.0_f32; 32_000];
        let resampled = resample_linear(&samples, 32_000, 24_000).unwrap();
        // The native resampler's exact framing may differ by a sample or two
        // from a naive ratio; assert it's in the right ballpark rather than
        // pinning an exact count owned by sherpa-onnx's implementation.
        assert!((resampled.len() as i64 - 24_000).abs() < 200);
    }

    #[test]
    fn resample_empty_input_stays_empty() {
        assert!(resample_linear(&[], 32_000, 24_000).unwrap().is_empty());
    }

    #[test]
    fn missing_wav_is_a_typed_error() {
        let err = load_voice_style(Path::new("/nonexistent/voice.wav")).unwrap_err();
        assert!(matches!(err, Error::Wav { .. }));
    }
}
