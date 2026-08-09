//! Loading a WAV file to stand in for live microphone capture in
//! `--simulate` mode.
//!
//! This development machine's audio input is a Jump Desktop virtual device
//! that captures digital silence -- there is no real microphone to hear
//! from here. `--simulate` proves the rest of the loop (AEC pairing, VAD,
//! STT, the session machine, the agent backend, TTS, and playback) is
//! genuinely wired end to end by injecting real speech audio at the exact
//! point live capture would otherwise arrive, shaped identically: 48 kHz
//! mono, chunked into `FRAME_SAMPLES`-length frames.

use std::path::{Path, PathBuf};

use buzztalk_core::FRAME_SAMPLES;

use crate::error::PipelineError;
use crate::playback::resample_linear_to_48k;

/// How much silence to append after the WAV's real content, in frames at
/// [`buzztalk_core::FRAME_MS`]. Real speech recordings don't reliably end
/// with enough trailing silence for the endpoint detector's hangover to
/// fire on their own; padding guarantees `--simulate` reaches a genuine
/// `SpeechEnd` rather than relying on the (much longer) `MAX_UTTERANCE`
/// fallback timeout.
const TRAILING_SILENCE_FRAMES: usize = 100; // ~1s at the 10ms frame cadence

/// Load `path` as mono audio, resampled (if necessary) to
/// [`buzztalk_core::SAMPLE_RATE_HZ`], chunked into exact [`FRAME_SAMPLES`]
/// frames -- the same shape live capture delivers -- and padded with
/// trailing silence so a normal endpoint can fire.
pub(crate) fn load_as_capture_frames(path: &Path) -> Result<Vec<Vec<f32>>, PipelineError> {
    let mut reader = hound::WavReader::open(path).map_err(|source| PipelineError::SimulateWav {
        path: path.to_path_buf(),
        source,
    })?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(PipelineError::SimulateWavNoChannels(path.to_path_buf()));
    }

    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| PipelineError::SimulateWav {
                path: path.to_path_buf(),
                source,
            })?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample.saturating_sub(1))).max(1) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| PipelineError::SimulateWav {
                    path: path.to_path_buf(),
                    source,
                })?
        }
    };

    // Fold to mono if the file happens to be multi-channel. The shipped
    // Parakeet test WAVs are mono; this just keeps a stray stereo file from
    // panicking instead of degrading gracefully.
    let mono: Vec<f32> = if spec.channels == 1 {
        raw
    } else {
        raw.chunks(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    };

    let mut at_48k = resample_linear_to_48k(&mono, spec.sample_rate);
    at_48k.extend(std::iter::repeat_n(
        0.0_f32,
        TRAILING_SILENCE_FRAMES * FRAME_SAMPLES,
    ));

    let mut frames = Vec::with_capacity(at_48k.len() / FRAME_SAMPLES + 1);
    for chunk in at_48k.chunks(FRAME_SAMPLES) {
        let mut frame = vec![0.0_f32; FRAME_SAMPLES];
        frame[..chunk.len()].copy_from_slice(chunk);
        frames.push(frame);
    }
    Ok(frames)
}

/// The default `--simulate` WAV: the Parakeet test fixture shipped with the
/// model bundle.
pub(crate) fn default_simulate_wav() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".buzztalk/models/parakeet-tdt-ctc-110m-en/test_wavs/0.wav")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_wav(path: &Path, sample_rate: u32, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn loaded_frames_are_all_exactly_frame_samples_long() {
        let dir = std::env::temp_dir().join("buzztalk-pipeline-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone_16k.wav");
        let samples: Vec<i16> = (0..1600).map(|i| ((i % 200) * 100) as i16).collect();
        write_test_wav(&path, 16_000, &samples);

        let frames = load_as_capture_frames(&path).expect("should load");
        assert!(!frames.is_empty());
        for frame in &frames {
            assert_eq!(frame.len(), FRAME_SAMPLES);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trailing_silence_is_appended() {
        let dir = std::env::temp_dir().join("buzztalk-pipeline-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("brief_16k.wav");
        write_test_wav(&path, 16_000, &[1000; 100]);

        let frames = load_as_capture_frames(&path).expect("should load");
        let last = frames.last().expect("at least one frame");
        assert!(
            last.iter().all(|&s| s == 0.0),
            "the last frame should be pure trailing silence"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_reports_a_pipeline_error_not_a_panic() {
        let err = load_as_capture_frames(Path::new("/definitely/not/a/real/file.wav"));
        assert!(err.is_err());
    }

    #[test]
    fn default_simulate_wav_points_at_the_parakeet_test_fixture() {
        let path = default_simulate_wav();
        assert!(path.ends_with("test_wavs/0.wav"));
    }

    /// Just a sanity check that the writer helper above round-trips through
    /// hound the way the loader expects (belt-and-braces on the test
    /// fixture itself, not the production code).
    #[test]
    fn test_wav_writer_helper_produces_a_readable_file() {
        use std::io::Read as _;

        let dir = std::env::temp_dir().join("buzztalk-pipeline-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sanity.wav");
        write_test_wav(&path, 16_000, &[42; 10]);
        let mut f = std::fs::File::open(&path).unwrap();
        let mut buf = [0u8; 4];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"RIFF");
        let _ = std::fs::remove_file(&path);
    }
}
