//! Offline-model speech recognition for BuzzTalk.
//!
//! The only STT model BuzzTalk ships (NVIDIA Parakeet TDT-CTC 110M, INT8) is
//! an *offline* model: it has no incremental/streaming decode API, so a full
//! re-decode is the unit of work. [`ParakeetRecognizer`] turns that into
//! pseudo-streaming by re-decoding a growing buffer on a timer and reporting
//! how much of each new partial agrees with the previous one, so a caller can
//! render the settled prefix differently from the still-shifting tail.
//!
//! This crate has no UI and no audio-device I/O: it consumes 16 kHz mono
//! `f32` samples and emits [`Transcript`] values. [`Resampler48to16`] exists
//! because the rest of BuzzTalk runs its pipeline at
//! [`buzztalk_core::SAMPLE_RATE_HZ`] (48 kHz).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod model;
mod recognizer;
mod resample;

pub use model::{default_model_dir, ModelError};
pub use recognizer::ParakeetRecognizer;
pub use resample::Resampler48to16;

use buzztalk_core::Result;

/// A speech-recognition result surfaced as audio streams in.
#[derive(Debug, Clone, PartialEq)]
pub enum Transcript {
    /// A tentative transcript for a still-open utterance.
    Partial {
        /// Best-guess text for the whole utterance decoded so far.
        text: String,
        /// Byte length of the prefix shared with the *previous* `Partial`
        /// emitted for this utterance (see [`SpeechRecognizer::push_audio`]).
        ///
        /// Re-decoding a growing buffer means the tail of the text is
        /// unstable — later context can change how the last word or two get
        /// transcribed. `text[..stable_prefix_len]` is safe to render as
        /// settled; the remainder should be treated as still shifting. This
        /// is a plain longest-common-prefix measure, not a model confidence
        /// score.
        stable_prefix_len: usize,
    },
    /// The finished transcript for a closed utterance.
    Final {
        /// Text decoded from the entire utterance buffer.
        text: String,
    },
}

/// A speech recognizer that consumes 16 kHz mono audio and emits transcripts.
///
/// Implementations are free to buffer internally; the only contract is what
/// each method promises below.
pub trait SpeechRecognizer: Send {
    /// Push more 16 kHz mono audio belonging to the current utterance.
    ///
    /// Returns `Ok(Some(Transcript::Partial { .. }))` when enough new audio
    /// has accumulated to justify a re-decode, `Ok(None)` otherwise.
    fn push_audio(&mut self, samples_16k: &[f32]) -> Result<Option<Transcript>>;

    /// End the current utterance: decode everything buffered since the last
    /// [`reset`](SpeechRecognizer::reset) or `finish_utterance` call, emit a
    /// `Transcript::Final`, and clear state for the next utterance.
    ///
    /// Returns `Ok(None)` if no audio was buffered.
    fn finish_utterance(&mut self) -> Result<Option<Transcript>>;

    /// Discard all buffered audio and partial state without emitting
    /// anything. Called on turn boundaries and barge-in.
    fn reset(&mut self);
}
