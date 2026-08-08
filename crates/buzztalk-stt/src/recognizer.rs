//! [`ParakeetRecognizer`]: pseudo-streaming partials over an offline CTC
//! model.

use std::path::Path;

use buzztalk_core::Result as CoreResult;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};

use crate::model::{default_model_dir, resolve_model_paths, ModelError};
use crate::{SpeechRecognizer, Transcript};

/// Sample rate the model and this recognizer operate at. Not
/// [`buzztalk_core::SAMPLE_RATE_HZ`] — see [`crate::Resampler48to16`] for the
/// 48 kHz -> 16 kHz conversion that happens before audio reaches here.
const SAMPLE_RATE_16K: i32 = 16_000;

/// How much new audio triggers a re-decode, in the common case.
const PARTIAL_INTERVAL_MS: u64 = 320;
/// How much new audio triggers a re-decode once the buffer is long (see
/// [`BACKOFF_THRESHOLD_SECS`]). Longer utterances back off the re-decode rate
/// because each re-decode re-processes the whole (capped) buffer, so cost
/// grows with buffer length even though the interval doesn't.
const PARTIAL_INTERVAL_BACKOFF_MS: u64 = 500;
/// Buffer length past which the backed-off interval applies.
const BACKOFF_THRESHOLD_SECS: u64 = 6;
/// Maximum buffer length kept for re-decoding. Older audio is dropped from
/// the front so a long utterance doesn't make every re-decode more expensive
/// without bound. `finish_utterance` only ever sees this capped window too —
/// for BuzzTalk's turn lengths that's an acceptable trade against unbounded
/// per-partial cost.
const MAX_WINDOW_SECS: u64 = 8;

const PARTIAL_INTERVAL_SAMPLES: usize =
    (SAMPLE_RATE_16K as u64 * PARTIAL_INTERVAL_MS / 1000) as usize;
const PARTIAL_INTERVAL_BACKOFF_SAMPLES: usize =
    (SAMPLE_RATE_16K as u64 * PARTIAL_INTERVAL_BACKOFF_MS / 1000) as usize;
const BACKOFF_THRESHOLD_SAMPLES: usize = SAMPLE_RATE_16K as usize * BACKOFF_THRESHOLD_SECS as usize;
const MAX_WINDOW_SAMPLES: usize = SAMPLE_RATE_16K as usize * MAX_WINDOW_SECS as usize;

/// Accumulation, capping, and re-decode-timing logic for pseudo-streaming.
///
/// Deliberately has no dependency on sherpa-onnx or any model: it is the
/// part of [`ParakeetRecognizer`] that unit tests can exercise without a
/// model on disk.
pub(crate) struct StreamBuffer {
    samples: Vec<f32>,
    since_last_partial: usize,
}

impl StreamBuffer {
    pub(crate) fn new() -> Self {
        Self {
            samples: Vec::new(),
            since_last_partial: 0,
        }
    }

    /// Append audio, dropping from the front if the buffer exceeds
    /// [`MAX_WINDOW_SAMPLES`].
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.samples.extend_from_slice(samples);
        self.since_last_partial += samples.len();
        if self.samples.len() > MAX_WINDOW_SAMPLES {
            let excess = self.samples.len() - MAX_WINDOW_SAMPLES;
            self.samples.drain(0..excess);
        }
    }

    /// The re-decode interval that currently applies, in samples.
    pub(crate) fn partial_interval_samples(&self) -> usize {
        if self.samples.len() > BACKOFF_THRESHOLD_SAMPLES {
            PARTIAL_INTERVAL_BACKOFF_SAMPLES
        } else {
            PARTIAL_INTERVAL_SAMPLES
        }
    }

    /// Whether enough new audio has arrived since the last partial to
    /// justify a re-decode.
    pub(crate) fn should_emit_partial(&self) -> bool {
        self.since_last_partial >= self.partial_interval_samples()
    }

    /// Record that a partial was just emitted, resetting the interval
    /// counter (but not the buffered audio).
    pub(crate) fn mark_partial_emitted(&mut self) {
        self.since_last_partial = 0;
    }

    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.samples
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.samples.clear();
        self.since_last_partial = 0;
    }
}

/// Byte length of the longest common prefix of two strings, split on `char`
/// boundaries so the result is always safe to slice a `&str` with.
///
/// This is a plain textual comparison — not a confidence score, not aligned
/// to word boundaries. A UI wanting word-stable rendering should snap this
/// down to the last preceding whitespace itself.
pub(crate) fn longest_common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

/// Speech recognizer over NVIDIA Parakeet TDT-CTC 110M (English, INT8), an
/// offline (non-streaming) NeMo CTC model, run through sherpa-onnx.
///
/// The model has no incremental decode API, so [`push_audio`](Self::push_audio)
/// re-decodes a growing, capped buffer on a timer (see the module docs on
/// [`StreamBuffer`]) rather than truly streaming.
pub struct ParakeetRecognizer {
    recognizer: OfflineRecognizer,
    stream: StreamBuffer,
    last_partial_text: String,
}

impl ParakeetRecognizer {
    /// Load the model from [`default_model_dir`] (honouring
    /// `BUZZTALK_MODEL_DIR`).
    pub fn with_default_model() -> Result<Self, ModelError> {
        Self::new(&default_model_dir())
    }

    /// Load the model from an explicit directory containing
    /// `model.int8.onnx` and `tokens.txt`.
    pub fn new(model_dir: &Path) -> Result<Self, ModelError> {
        let paths = resolve_model_paths(model_dir)?;

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.nemo_ctc.model = Some(paths.model.to_string_lossy().into_owned());
        config.model_config.tokens = Some(paths.tokens.to_string_lossy().into_owned());
        config.model_config.num_threads = 1;

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| ModelError::CreateFailed(model_dir.to_path_buf()))?;

        Ok(Self {
            recognizer,
            stream: StreamBuffer::new(),
            last_partial_text: String::new(),
        })
    }

    /// Decode the current buffer contents in one shot and return the text,
    /// trimmed of the leading/trailing whitespace sherpa-onnx sometimes
    /// includes.
    fn decode_buffer(&self) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE_16K, self.stream.as_slice());
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}

impl SpeechRecognizer for ParakeetRecognizer {
    fn push_audio(&mut self, samples_16k: &[f32]) -> CoreResult<Option<Transcript>> {
        self.stream.push(samples_16k);

        if !self.stream.should_emit_partial() {
            return Ok(None);
        }
        self.stream.mark_partial_emitted();

        let text = self.decode_buffer();
        let stable_prefix_len = longest_common_prefix_len(&self.last_partial_text, &text);
        self.last_partial_text = text.clone();

        Ok(Some(Transcript::Partial {
            text,
            stable_prefix_len,
        }))
    }

    fn finish_utterance(&mut self) -> CoreResult<Option<Transcript>> {
        if self.stream.is_empty() {
            self.reset();
            return Ok(None);
        }
        let text = self.decode_buffer();
        self.reset();
        Ok(Some(Transcript::Final { text }))
    }

    fn reset(&mut self) {
        self.stream.clear();
        self.last_partial_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── longest_common_prefix_len ───────────────────────────────────────

    #[test]
    fn lcp_identical_strings() {
        assert_eq!(longest_common_prefix_len("hello world", "hello world"), 11);
    }

    #[test]
    fn lcp_diverging_tail() {
        // "hello wor" is the shared prefix (9 bytes, all ASCII).
        assert_eq!(longest_common_prefix_len("hello world", "hello worked"), 9);
    }

    #[test]
    fn lcp_empty_previous_is_zero() {
        assert_eq!(longest_common_prefix_len("", "hello"), 0);
    }

    #[test]
    fn lcp_no_shared_prefix() {
        assert_eq!(longest_common_prefix_len("abc", "xyz"), 0);
    }

    #[test]
    fn lcp_respects_char_boundaries_for_multibyte_utf8() {
        // "café" vs "cafe": shared prefix is "caf" (3 ASCII bytes); the 'é'
        // vs 'e' divergence must not slice a multi-byte char in half.
        let len = longest_common_prefix_len("café", "cafe");
        assert_eq!(len, 3);
        assert!("café".is_char_boundary(len));
    }

    // ── StreamBuffer: capping and interval backoff ──────────────────────

    #[test]
    fn buffer_caps_to_max_window() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; MAX_WINDOW_SAMPLES + 1000]);
        assert_eq!(buf.as_slice().len(), MAX_WINDOW_SAMPLES);
    }

    #[test]
    fn buffer_cap_keeps_the_most_recent_audio() {
        let mut buf = StreamBuffer::new();
        // Push a marker value, then pad past the cap; the marker should be
        // the sample dropped from the front.
        buf.push(&[9.0]);
        buf.push(&vec![0.0; MAX_WINDOW_SAMPLES]);
        assert_eq!(buf.as_slice().len(), MAX_WINDOW_SAMPLES);
        assert!(buf.as_slice().iter().all(|&s| s == 0.0));
    }

    #[test]
    fn interval_is_normal_below_backoff_threshold() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; BACKOFF_THRESHOLD_SAMPLES]);
        assert_eq!(buf.partial_interval_samples(), PARTIAL_INTERVAL_SAMPLES);
    }

    #[test]
    fn interval_backs_off_above_threshold() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; BACKOFF_THRESHOLD_SAMPLES + 1]);
        assert_eq!(
            buf.partial_interval_samples(),
            PARTIAL_INTERVAL_BACKOFF_SAMPLES
        );
    }

    #[test]
    fn should_emit_partial_respects_interval() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; PARTIAL_INTERVAL_SAMPLES - 1]);
        assert!(!buf.should_emit_partial());
        buf.push(&[0.0]);
        assert!(buf.should_emit_partial());
    }

    #[test]
    fn mark_partial_emitted_resets_only_the_interval_counter() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; PARTIAL_INTERVAL_SAMPLES]);
        assert!(buf.should_emit_partial());
        buf.mark_partial_emitted();
        assert!(!buf.should_emit_partial());
        // Buffered audio itself is untouched.
        assert_eq!(buf.as_slice().len(), PARTIAL_INTERVAL_SAMPLES);
    }

    #[test]
    fn clear_resets_everything() {
        let mut buf = StreamBuffer::new();
        buf.push(&vec![0.0; PARTIAL_INTERVAL_SAMPLES]);
        buf.clear();
        assert!(buf.is_empty());
        assert!(!buf.should_emit_partial());
    }

    // ── Model discovery errors (no model required) ──────────────────────

    #[test]
    fn new_reports_missing_model_dir_without_panicking() {
        // `ParakeetRecognizer` has no `Debug` impl (its sherpa-onnx handle
        // doesn't provide one), so match instead of `.expect_err()`.
        match ParakeetRecognizer::new(Path::new("/definitely/not/a/real/model/dir")) {
            Err(ModelError::DirNotFound(_)) => {}
            Ok(_) => panic!("expected an error, got Ok"),
            Err(other) => panic!("expected DirNotFound, got {other}"),
        }
    }

    // ── Integration: requires the real model on disk ────────────────────

    #[test]
    #[ignore = "requires the real Parakeet model on disk; run with --ignored"]
    fn transcribes_real_test_wav_and_reports_timing() {
        use sherpa_onnx::Wave;
        use std::time::Instant;

        let mut recognizer = ParakeetRecognizer::with_default_model()
            .expect("model should be present for this test");

        let model_dir = default_model_dir();
        let wav_path = model_dir.join("test_wavs").join("0.wav");
        let wave = Wave::read(wav_path.to_str().expect("valid utf8 path")).expect("read test wav");
        assert_eq!(
            wave.sample_rate(),
            SAMPLE_RATE_16K,
            "test wav must already be 16 kHz"
        );

        let start = Instant::now();
        recognizer
            .push_audio(wave.samples())
            .expect("push_audio does not error");
        let transcript = recognizer
            .finish_utterance()
            .expect("finish_utterance does not error")
            .expect("non-empty buffer should produce a Final transcript");
        let elapsed = start.elapsed();

        let text = match transcript {
            Transcript::Final { text } => text,
            other => panic!("expected Transcript::Final, got {other:?}"),
        };

        println!(
            "test_wavs/0.wav ({:.2}s audio) decoded in {:?}: {text:?}",
            wave.samples().len() as f32 / SAMPLE_RATE_16K as f32,
            elapsed
        );

        assert!(!text.trim().is_empty(), "transcript should not be empty");
    }

    #[test]
    #[ignore = "requires the real Parakeet model on disk; run with --ignored"]
    fn pseudo_streaming_emits_growing_partials_from_chunked_audio() {
        use sherpa_onnx::Wave;

        let mut recognizer = ParakeetRecognizer::with_default_model()
            .expect("model should be present for this test");

        let model_dir = default_model_dir();
        let wav_path = model_dir.join("test_wavs").join("0.wav");
        let wave = Wave::read(wav_path.to_str().expect("valid utf8 path")).expect("read test wav");

        let mut partial_count = 0;
        for chunk in wave.samples().chunks(1600) {
            // 100 ms chunks, well under the 320 ms partial interval, so this
            // exercises push_audio's "not yet" path as well as its "emit"
            // path across several re-decodes.
            if let Some(Transcript::Partial {
                text,
                stable_prefix_len,
            }) = recognizer
                .push_audio(chunk)
                .expect("push_audio does not error")
            {
                partial_count += 1;
                println!(
                    "partial #{partial_count} (stable_prefix_len={stable_prefix_len}): {text:?}"
                );
                assert!(
                    stable_prefix_len <= text.len(),
                    "stable_prefix_len must be a valid byte index into text"
                );
            }
        }

        let final_transcript = recognizer
            .finish_utterance()
            .expect("finish_utterance does not error")
            .expect("non-empty buffer should produce a Final transcript");
        let final_text = match final_transcript {
            Transcript::Final { text } => text,
            other => panic!("expected Transcript::Final, got {other:?}"),
        };
        println!("final: {final_text:?}");

        assert!(
            partial_count > 0,
            "a 7.4s utterance fed in 100ms chunks should cross the 320ms partial interval at least once"
        );
        assert!(!final_text.trim().is_empty());
    }

    #[test]
    #[ignore = "requires the real Parakeet model on disk; run with --ignored"]
    fn measures_redecode_cost_on_a_three_second_buffer() {
        use std::time::Instant;

        let recognizer = ParakeetRecognizer::with_default_model()
            .expect("model should be present for this test");

        // Three seconds of silence is enough to measure re-decode cost: the
        // model still has to run its full forward pass over the window
        // regardless of content.
        let three_seconds = vec![0.0_f32; SAMPLE_RATE_16K as usize * 3];

        let stream = recognizer.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE_16K, &three_seconds);

        let start = Instant::now();
        recognizer.recognizer.decode(&stream);
        let _ = stream.get_result();
        let elapsed = start.elapsed();

        println!("single re-decode of a 3s buffer took {elapsed:?}");
    }
}
