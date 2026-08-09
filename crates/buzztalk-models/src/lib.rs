//! Downloads and verifies the speech model bundles BuzzTalk needs, so a
//! freshly built or freshly downloaded binary works without anyone having
//! to `curl` model files into place by hand first.
//!
//! Two bundles, both installed under [`models_dir`] (default
//! `~/.buzztalk/models`, override with `BUZZTALK_MODELS_DIR`):
//!
//! - **STT**: sherpa-onnx's Parakeet TDT-CTC 110M (English), int8 --
//!   fetched as a single `.tar.bz2` archive and extracted.
//! - **TTS**: Kyutai's Pocket TTS (`english_2026-04`), the KevinAHM ONNX
//!   export -- fetched as ten individual files.
//!
//! Both defaults line up with what `buzztalk-stt` and `buzztalk-tts`
//! already look for (`BUZZTALK_MODEL_DIR` / `BUZZTALK_TTS_MODEL_DIR`), so
//! installing here with no further configuration is enough for both
//! crates to find their model on a fresh machine.
//!
//! ## Integrity discipline
//!
//! Every artifact's SHA-256 is pinned in [`manifest`] and verified against
//! the downloaded bytes *before* anything is written to its final
//! location. A hash mismatch is refused, not warned about -- see
//! [`Error::HashMismatch`]. Installs are atomic (a half-downloaded file or
//! half-extracted archive never appears at its final path -- see
//! [`install`]'s module docs) and idempotent (a file already present with
//! the correct hash is not re-downloaded).
//!
//! ## Quick start
//!
//! ```no_run
//! use buzztalk_models::{ensure_models, status, ModelSet};
//!
//! // Fetch both bundles, printing progress to stderr.
//! ensure_models(ModelSet::All).expect("model download failed");
//!
//! // Or just check what's there.
//! println!("{}", status());
//! ```

#![warn(missing_docs)]

mod error;
mod fetch;
mod hash;
mod install;
mod manifest;
mod paths;
mod progress;
mod status;

pub use error::{Error, Result};
pub use fetch::{Fetcher, UreqFetcher};
pub use install::{ensure_models_at, ModelSet};
pub use paths::{models_dir, MODELS_DIR_ENV_VAR};
pub use progress::{default_printer, human_bytes, ProgressEvent};
pub use status::{status_at, BundleStatus, ModelStatus};

/// Download and verify whichever bundle(s) `which` selects into
/// [`models_dir`], printing progress to stderr as it goes.
///
/// This is the real-world entry point: it uses the actual network
/// ([`UreqFetcher`]) and the actual resolved models directory. For tests,
/// use [`ensure_models_at`] directly with a temp directory and a fake
/// [`Fetcher`] -- no test in this crate's own suite calls this function
/// (see `tests/live_download.rs`, which is `#[ignore]`d).
pub fn ensure_models(which: ModelSet) -> Result<()> {
    let base_dir = models_dir();
    let fetcher = UreqFetcher;
    let mut on_progress = default_printer();
    install::ensure_models_at(which, &base_dir, &fetcher, &mut |event| on_progress(event))
}

/// Report what's present, what's missing, and how much disk space both
/// bundles are using under [`models_dir`].
pub fn status() -> ModelStatus {
    status::status_at(&models_dir())
}
