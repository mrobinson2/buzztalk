//! Model discovery: where the Pocket TTS bundle lives on disk.

use std::path::PathBuf;

/// Environment variable that overrides [`default_model_dir`].
pub const MODEL_DIR_ENV_VAR: &str = "BUZZTALK_TTS_MODEL_DIR";

/// Resolve the directory containing the Pocket TTS bundle.
///
/// Honours [`MODEL_DIR_ENV_VAR`] (`BUZZTALK_TTS_MODEL_DIR`) if set, non-empty,
/// and pointing anywhere at all — callers find out it's *wrong* (missing
/// files) from a typed [`crate::Error`] when they actually try to load it,
/// not from this function silently falling back.
///
/// Otherwise defaults to `~/.buzztalk/models/pocket-tts`, resolved from the
/// `HOME` environment variable. If `HOME` is unset, falls back to
/// `./.buzztalk/models/pocket-tts` relative to the current directory rather
/// than panicking.
pub fn default_model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODEL_DIR_ENV_VAR) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    // The shared base that `buzztalk-models` downloads into, and that
    // `--model-status` reports on. Without honouring it, the status command
    // would describe a directory the synthesizer never reads — a mismatch that
    // looks like working software right up until someone relies on it.
    if let Some(base) = std::env::var_os("BUZZTALK_MODELS_DIR") {
        if !base.is_empty() {
            return PathBuf::from(base).join("pocket-tts");
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".buzztalk").join("models").join("pocket-tts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env` is process-global; cargo runs tests in this module on
    // multiple threads by default. Serialize the three tests that touch
    // HOME / BUZZTALK_TTS_MODEL_DIR so they don't race each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_to_home_dot_buzztalk_models_pocket_tts() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK above.
        unsafe {
            std::env::remove_var(MODEL_DIR_ENV_VAR);
            std::env::set_var("HOME", "/home/example");
        }
        assert_eq!(
            default_model_dir(),
            PathBuf::from("/home/example/.buzztalk/models/pocket-tts")
        );
    }

    #[test]
    fn override_env_var_wins() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK above.
        unsafe {
            std::env::set_var(MODEL_DIR_ENV_VAR, "/custom/models/pocket-tts");
        }
        assert_eq!(
            default_model_dir(),
            PathBuf::from("/custom/models/pocket-tts")
        );
        unsafe {
            std::env::remove_var(MODEL_DIR_ENV_VAR);
        }
    }

    #[test]
    fn empty_override_is_ignored() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK above.
        unsafe {
            std::env::set_var(MODEL_DIR_ENV_VAR, "");
            std::env::set_var("HOME", "/home/example");
        }
        assert_eq!(
            default_model_dir(),
            PathBuf::from("/home/example/.buzztalk/models/pocket-tts")
        );
        unsafe {
            std::env::remove_var(MODEL_DIR_ENV_VAR);
        }
    }
}
