//! Errors surfaced by `buzztalk-pipeline`.

use std::path::PathBuf;

/// Errors that can occur while constructing or running a
/// [`crate::ConversationPipeline`].
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The duplex audio engine could not be opened.
    #[error("audio engine: {0}")]
    Audio(#[from] buzztalk_core::Error),

    /// The Parakeet speech recognizer could not be loaded.
    #[error("speech recognizer: {0}")]
    Stt(#[from] buzztalk_stt::ModelError),

    /// The Pocket TTS synthesizer could not be loaded.
    #[error("speech synthesizer: {0}")]
    Tts(#[from] buzztalk_tts::Error),

    /// `--simulate`'s WAV file could not be read.
    #[error("could not read simulated capture WAV at {path}: {source}")]
    SimulateWav {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying `hound` error.
        #[source]
        source: hound::Error,
    },

    /// `--simulate`'s WAV file reported zero channels.
    #[error("simulated capture WAV at {} has zero channels", .0.display())]
    SimulateWavNoChannels(PathBuf),
}
