//! Model discovery and validation for [`crate::ParakeetRecognizer`].

use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";

/// Errors that can occur while locating or loading the STT model.
///
/// This is a normal, recoverable condition — a missing or partially
/// downloaded model directory — not a programming error, so it is a typed
/// value rather than a panic.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ModelError {
    /// The model directory does not exist (or is not a directory).
    #[error("model directory not found: {}", .0.display())]
    DirNotFound(PathBuf),

    /// The model directory exists but a required file is missing from it.
    #[error("missing required model file: {}", .0.display())]
    MissingFile(PathBuf),

    /// sherpa-onnx rejected the model files (corrupt, wrong format, or the
    /// native library could not initialize the ONNX Runtime session).
    #[error("sherpa-onnx failed to create the offline recognizer from {}", .0.display())]
    CreateFailed(PathBuf),
}

/// The default model directory: `~/.buzztalk/models/parakeet-tdt-ctc-110m-en`,
/// or the value of `BUZZTALK_MODEL_DIR` if set.
///
/// This only computes the path; it does not check that anything exists
/// there. [`crate::ParakeetRecognizer::new`] does that and returns a
/// [`ModelError`] if the directory or its files are missing.
pub fn default_model_dir() -> PathBuf {
    model_dir_from(
        env::var_os("BUZZTALK_MODEL_DIR").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
    )
}

/// Pure resolution logic behind [`default_model_dir`], split out so it is
/// testable without mutating process-global environment state (`env::set_var`
/// requires `unsafe` as of recent `std`, and this crate forbids `unsafe`).
fn model_dir_from(env_override: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = env_override {
        return dir;
    }
    let home = home.unwrap_or_else(|| PathBuf::from("."));
    home.join(".buzztalk/models/parakeet-tdt-ctc-110m-en")
}

/// Resolved, validated paths to the model's on-disk files.
pub(crate) struct ModelPaths {
    pub model: PathBuf,
    pub tokens: PathBuf,
}

/// Validate that `dir` exists and contains the files sherpa-onnx needs.
pub(crate) fn resolve_model_paths(dir: &Path) -> Result<ModelPaths, ModelError> {
    if !dir.is_dir() {
        return Err(ModelError::DirNotFound(dir.to_path_buf()));
    }
    let model = dir.join(MODEL_FILE);
    if !model.is_file() {
        return Err(ModelError::MissingFile(model));
    }
    let tokens = dir.join(TOKENS_FILE);
    if !tokens.is_file() {
        return Err(ModelError::MissingFile(tokens));
    }
    Ok(ModelPaths { model, tokens })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_honours_env_override() {
        let dir = model_dir_from(
            Some(PathBuf::from("/tmp/some-other-model-dir")),
            Some(PathBuf::from("/home/someone")),
        );
        assert_eq!(dir, PathBuf::from("/tmp/some-other-model-dir"));
    }

    #[test]
    fn model_dir_falls_back_to_home_when_no_override() {
        let dir = model_dir_from(None, Some(PathBuf::from("/home/someone")));
        assert_eq!(
            dir,
            PathBuf::from("/home/someone/.buzztalk/models/parakeet-tdt-ctc-110m-en")
        );
    }

    #[test]
    fn default_model_dir_ends_with_expected_model_name() {
        // Exercises the real env::var_os wiring, without mutating it.
        let dir = default_model_dir();
        assert!(dir.ends_with("parakeet-tdt-ctc-110m-en"));
    }

    #[test]
    fn resolve_model_paths_rejects_missing_dir() {
        // `ModelPaths` has no `Debug` impl (it's an internal detail), so
        // match instead of `.unwrap_err()`.
        match resolve_model_paths(Path::new("/definitely/not/a/real/path")) {
            Err(ModelError::DirNotFound(_)) => {}
            Ok(_) => panic!("expected an error, got Ok"),
            Err(other) => panic!("expected DirNotFound, got {other}"),
        }
    }

    #[test]
    fn resolve_model_paths_rejects_dir_missing_files() {
        let dir = std::env::temp_dir().join("buzztalk-stt-test-empty-model-dir");
        let _ = std::fs::create_dir_all(&dir);
        match resolve_model_paths(&dir) {
            Err(ModelError::MissingFile(_)) => {}
            Ok(_) => panic!("expected an error, got Ok"),
            Err(other) => panic!("expected MissingFile, got {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
