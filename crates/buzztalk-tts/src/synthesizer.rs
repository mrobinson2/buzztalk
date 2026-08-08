//! [`SpeechSynthesizer`] wiring: loads the Pocket TTS bundle, holds the
//! reference voice, and turns each already-segmented phrase (see
//! [`crate::segmentation`]) into one [`AudioChunk`].

use std::path::Path;

use crate::bundle;
use crate::engine::PocketEngine;
use crate::error::Result;
use crate::wav::{load_voice_style, VoiceStyle};

/// A single synthesized span of audio.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Mono PCM samples in `[-1, 1]`.
    pub samples: Vec<f32>,
    /// Sample rate of `samples`, in Hz. Pocket TTS emits 24 kHz.
    pub sample_rate: u32,
    /// Position of this chunk within an ongoing synthesis sequence,
    /// starting at 0 and incrementing on every call to
    /// [`SpeechSynthesizer::synthesize`].
    pub chunk_index: usize,
}

/// A streaming-friendly text-to-speech engine.
///
/// Implementations are expected to hold one already-loaded model resident
/// in memory; `synthesize` is called once per phrase (see
/// [`crate::segmentation::segment_into_phrases`]) rather than once per
/// full response, so speech can start before generation finishes.
pub trait SpeechSynthesizer: Send {
    /// Synthesize one phrase-sized chunk of text.
    fn synthesize(&mut self, text: &str) -> Result<AudioChunk>;

    /// Run one throwaway synthesis to pay ONNX Runtime's first-inference
    /// cost up front. The first inference on a freshly loaded session is
    /// dramatically slower than subsequent ones; without a warmup call,
    /// the first thing a user hears is the slowest thing they will ever
    /// hear.
    fn warmup(&mut self) -> Result<()>;
}

/// [`SpeechSynthesizer`] backed by the Kyutai Pocket TTS `english_2026-04`
/// INT8 bundle, cloning the bundle's fixed reference voice.
pub struct PocketSynthesizer {
    engine: PocketEngine,
    voice: VoiceStyle,
    next_chunk_index: usize,
}

impl PocketSynthesizer {
    /// Load the engine and its bundled reference voice from `model_dir`.
    ///
    /// `num_threads` controls ONNX Runtime intra-op parallelism per
    /// session; `1` is a reasonable default for a synthesizer that will be
    /// driven from its own thread.
    pub fn load(model_dir: &Path, num_threads: usize) -> Result<Self> {
        let engine = PocketEngine::load(model_dir, num_threads)?;
        let voice = load_voice_style(&bundle::reference_voice_path(model_dir))?;
        Ok(Self {
            engine,
            voice,
            next_chunk_index: 0,
        })
    }

    /// The engine's output sample rate, in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }
}

impl SpeechSynthesizer for PocketSynthesizer {
    fn synthesize(&mut self, text: &str) -> Result<AudioChunk> {
        let samples = self.engine.synthesize_text(text, &self.voice)?;
        let chunk = AudioChunk {
            samples,
            sample_rate: self.engine.sample_rate(),
            chunk_index: self.next_chunk_index,
        };
        self.next_chunk_index += 1;
        Ok(chunk)
    }

    fn warmup(&mut self) -> Result<()> {
        // A short, fixed phrase: enough to run every session at least
        // once (voice encode is cached after the first call anyway) and
        // pay ONNX Runtime's first-inference cost, without depending on
        // whatever the caller's real first line turns out to be.
        self.engine.synthesize_text("Ready.", &self.voice)?;
        Ok(())
    }
}
