//! Where the model bundles live on disk.

use std::env;
use std::path::PathBuf;

/// Environment variable that overrides [`models_dir`].
pub const MODELS_DIR_ENV_VAR: &str = "BUZZTALK_MODELS_DIR";

/// The parent directory both model bundles install into:
/// `~/.buzztalk/models`, or [`MODELS_DIR_ENV_VAR`] if set and non-empty.
///
/// This is the *parent* of both bundle directories -- `buzztalk-stt`'s
/// `BUZZTALK_MODEL_DIR` and `buzztalk-tts`'s `BUZZTALK_TTS_MODEL_DIR`
/// already default to `models_dir().join("parakeet-tdt-ctc-110m-en")` and
/// `models_dir().join("pocket-tts")` respectively, so installing here with
/// no further configuration is enough for both crates to find their model
/// on a fresh machine.
pub fn models_dir() -> PathBuf {
    models_dir_from(
        env::var_os(MODELS_DIR_ENV_VAR).map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
    )
}

/// Pure resolution logic behind [`models_dir`], split out so it's testable
/// without mutating process-global environment state.
fn models_dir_from(env_override: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = env_override {
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    let home = home.unwrap_or_else(|| PathBuf::from("."));
    home.join(".buzztalk").join("models")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honours_env_override() {
        let dir = models_dir_from(
            Some(PathBuf::from("/tmp/some-other-models-dir")),
            Some(PathBuf::from("/home/someone")),
        );
        assert_eq!(dir, PathBuf::from("/tmp/some-other-models-dir"));
    }

    #[test]
    fn empty_env_override_is_ignored() {
        let dir = models_dir_from(Some(PathBuf::new()), Some(PathBuf::from("/home/someone")));
        assert_eq!(dir, PathBuf::from("/home/someone/.buzztalk/models"));
    }

    #[test]
    fn falls_back_to_home_when_no_override() {
        let dir = models_dir_from(None, Some(PathBuf::from("/home/someone")));
        assert_eq!(dir, PathBuf::from("/home/someone/.buzztalk/models"));
    }

    #[test]
    fn falls_back_to_cwd_when_no_home_either() {
        let dir = models_dir_from(None, None);
        assert_eq!(dir, PathBuf::from("./.buzztalk/models"));
    }
}
