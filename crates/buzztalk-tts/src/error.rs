//! Errors surfaced by `buzztalk-tts`.

use std::path::PathBuf;

/// Errors that can occur while locating, loading, or running Pocket TTS.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The configured model directory does not exist.
    #[error("Pocket TTS model directory not found: {0}")]
    ModelDirMissing(PathBuf),

    /// A file the bundle manifest requires is missing from the model
    /// directory.
    #[error("Pocket TTS bundle is missing required file `{file}` (expected at {path})")]
    MissingFile {
        /// Bundle-relative file name.
        file: &'static str,
        /// Full path that was checked.
        path: PathBuf,
    },

    /// A required file could not be read.
    #[error("could not read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// `bundle.json` could not be parsed.
    #[error("could not parse {path}: {source}")]
    Json {
        /// Path to `bundle.json`.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },

    /// `bundle.json` parsed but describes a bundle layout this crate does
    /// not support (schema version, dimensions, or prompt policy we were
    /// not built against).
    #[error("unsupported Pocket TTS bundle: {0}")]
    UnsupportedBundle(String),

    /// A `.npy` array (the voice BOS embedding) was not the expected
    /// little-endian float32 layout.
    #[error("invalid NumPy array at {path}: {reason}")]
    Npy {
        /// Path to the `.npy` file.
        path: PathBuf,
        /// Human-readable reason it was rejected.
        reason: String,
    },

    /// The SentencePiece tokenizer could not be loaded or converted.
    #[error("could not load Pocket TTS tokenizer: {0}")]
    Tokenizer(String),

    /// The reference voice WAV could not be read.
    #[error("could not read reference voice WAV at {path}: {source}")]
    Wav {
        /// Path to the WAV file.
        path: PathBuf,
        /// Underlying `hound` error.
        #[source]
        source: hound::Error,
    },

    /// The reference voice audio was empty or unusable.
    #[error("reference voice audio at {0} is empty")]
    EmptyVoice(PathBuf),

    /// An ONNX Runtime session failed to load or run.
    #[error("ONNX Runtime error while {context}: {source}")]
    Onnx {
        /// What we were doing when it failed, e.g. "loading mimi_encoder.onnx".
        context: String,
        /// Underlying `ort` error.
        #[source]
        source: ort::Error,
    },

    /// Synthesis failed for a reason not covered by a more specific variant
    /// (state-shape mismatches, empty inputs, EOS never reached, etc).
    #[error("Pocket TTS synthesis failed: {0}")]
    Synthesis(String),
}

/// Convenience alias, mirroring `buzztalk_core::Result` for this crate's
/// own error type.
pub type Result<T> = std::result::Result<T, Error>;
