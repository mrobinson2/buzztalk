//! Reference voice WAV loading and linear resampling.
//!
//! `buzz-voice` reads voice WAVs through `sherpa_onnx::Wave` and resamples
//! with `sherpa_onnx::LinearResampler`. This crate does not depend on
//! `sherpa-onnx` (see the crate-level docs for why), so this module reads
//! the bundle's single fixed reference WAV with `hound` instead, and the
//! resampler below is ported from `buzz-voice`'s `src/imported.rs`
//! (Apache-2.0) — see the crate-level attribution in `lib.rs`.

use std::path::Path;

use crate::error::{Error, Result};

/// Loaded reference voice samples, mono `f32` in `[-1, 1]`, at their
/// original sample rate.
#[derive(Debug, Clone)]
pub struct VoiceStyle {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Load a reference voice WAV, downmixing to mono if necessary.
pub fn load_voice_style(path: &Path) -> Result<VoiceStyle> {
    let reader = hound::WavReader::open(path).map_err(|source| Error::Wav {
        path: path.to_path_buf(),
        source,
    })?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let mono: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            let samples: std::result::Result<Vec<f32>, hound::Error> =
                reader.into_samples::<f32>().collect();
            let samples = samples.map_err(|source| Error::Wav {
                path: path.to_path_buf(),
                source,
            })?;
            downmix(&samples, channels)
        }
        hound::SampleFormat::Int => {
            let max_value = 2f32.powi(spec.bits_per_sample as i32 - 1);
            let samples: std::result::Result<Vec<i32>, hound::Error> =
                reader.into_samples::<i32>().collect();
            let samples = samples.map_err(|source| Error::Wav {
                path: path.to_path_buf(),
                source,
            })?;
            let normalized: Vec<f32> = samples
                .into_iter()
                .map(|sample| sample as f32 / max_value)
                .collect();
            downmix(&normalized, channels)
        }
    };

    if mono.is_empty() {
        return Err(Error::EmptyVoice(path.to_path_buf()));
    }

    Ok(VoiceStyle {
        samples: mono,
        sample_rate: spec.sample_rate,
    })
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Linearly resample `samples` from `source_rate` to `target_rate`.
///
/// Ported from `buzz-voice`'s `resample_linear` (`src/imported.rs`,
/// Apache-2.0), generalized to an explicit target rate instead of a
/// hardcoded constant.
pub fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == target_rate {
        return samples.to_vec();
    }
    let output_len = ((samples.len() as u64 * u64::from(target_rate) + u64::from(source_rate) / 2)
        / u64::from(source_rate)) as usize;
    (0..output_len)
        .map(|index| {
            let source = index as f64 * f64::from(source_rate) / f64::from(target_rate);
            let left = source.floor() as usize;
            let fraction = (source - left as f64) as f32;
            let a = samples[left.min(samples.len() - 1)];
            let b = samples[(left + 1).min(samples.len() - 1)];
            a + (b - a) * fraction
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_identity_when_rates_match() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&samples, 24_000, 24_000), samples);
    }

    #[test]
    fn resample_changes_length_proportionally() {
        let samples = vec![0.0_f32; 32_000];
        let resampled = resample_linear(&samples, 32_000, 24_000);
        assert_eq!(resampled.len(), 24_000);
    }

    #[test]
    fn resample_empty_input_stays_empty() {
        assert!(resample_linear(&[], 32_000, 24_000).is_empty());
    }

    #[test]
    fn downmix_averages_interleaved_channels() {
        // Two frames of stereo: (1.0, -1.0), (0.5, 0.5)
        let interleaved = vec![1.0, -1.0, 0.5, 0.5];
        assert_eq!(downmix(&interleaved, 2), vec![0.0, 0.5]);
    }

    #[test]
    fn downmix_mono_is_passthrough() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(downmix(&samples, 1), samples);
    }

    #[test]
    fn missing_wav_is_a_typed_error() {
        let err = load_voice_style(Path::new("/nonexistent/voice.wav")).unwrap_err();
        assert!(matches!(err, Error::Wav { .. }));
    }
}
