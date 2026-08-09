//! Pinned, known-good download specs for both model bundles.
//!
//! Every hash here was verified against the actual bytes before being
//! pinned (the TTS ones against the files already sitting in
//! `~/.buzztalk/models/pocket-tts` on the machine this crate was written
//! on, via `shasum -a 256`). Do not "fix" a hash here without re-verifying
//! it against a byte-identical copy of the real artifact -- that is
//! precisely the mistake this crate exists to make impossible for
//! everyone downstream.

/// One file to download: where it comes from, where it goes, and the
/// SHA-256 it must hash to before it's allowed to touch disk.
pub struct FileSpec {
    /// Full download URL.
    pub url: &'static str,
    /// Expected lowercase-hex SHA-256 of the downloaded bytes.
    pub sha256: &'static str,
    /// File name relative to the bundle's directory.
    pub file_name: &'static str,
}

// ---------------------------------------------------------------------
// STT: sherpa-onnx Parakeet TDT-CTC 110M (English), int8.
// ---------------------------------------------------------------------

/// Directory name (under [`crate::models_dir`]) the STT bundle installs
/// into. Matches `buzztalk-stt`'s own default (`~/.buzztalk/models/parakeet-tdt-ctc-110m-en`).
pub const STT_DIR_NAME: &str = "parakeet-tdt-ctc-110m-en";

/// Download URL for the STT archive.
pub const STT_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2";

/// Pinned SHA-256 of the STT archive itself (the `.tar.bz2`, before
/// extraction).
pub const STT_ARCHIVE_SHA256: &str =
    "17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b";

/// The directory name the archive expands to internally, before it gets
/// renamed to [`STT_DIR_NAME`].
pub const STT_ARCHIVE_TOP_DIR: &str = "sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8";

/// Files that must be present directly inside [`STT_DIR_NAME`] for the
/// bundle to count as installed -- mirrors what `buzztalk-stt::model`
/// requires (`model.int8.onnx`, `tokens.txt`), plus the license file this
/// crate writes itself.
pub const STT_REQUIRED_FILES: &[&str] = &["model.int8.onnx", "tokens.txt", "MODEL_LICENSE.txt"];

/// NVIDIA's CC-BY-4.0 attribution notice for the Parakeet model, written
/// into [`STT_DIR_NAME`] on every install. Embedded at compile time from
/// `assets/MODEL_LICENSE.txt`, an exact byte-for-byte copy of the notice
/// already shipped by the manual install this crate replaces -- CC-BY-4.0
/// requires attribution to travel with the bytes, so this is a license
/// obligation, not a nicety.
pub const STT_MODEL_LICENSE_TEXT: &str = include_str!("../assets/MODEL_LICENSE.txt");

/// File name the license notice is written to inside [`STT_DIR_NAME`].
pub const STT_MODEL_LICENSE_FILE: &str = "MODEL_LICENSE.txt";

// ---------------------------------------------------------------------
// TTS: Kyutai Pocket TTS (english_2026-04), KevinAHM ONNX export.
// ---------------------------------------------------------------------

/// Directory name (under [`crate::models_dir`]) the TTS bundle installs
/// into. Matches `buzztalk-tts`'s own default (`~/.buzztalk/models/pocket-tts`).
pub const TTS_DIR_NAME: &str = "pocket-tts";

/// Base URL every [`TTS_FILES`] entry except `LICENSE` and
/// `reference_sample.wav` is served from. Not used to build URLs at
/// runtime (the full URLs are spelled out below so they stay `&'static
/// str` literals) -- kept as a named constant purely so the relationship
/// is documented and grep-able.
#[allow(dead_code)]
const TTS_BASE_URL: &str = "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/";

/// The bundle files served from [`TTS_BASE_URL`], plus the two files that
/// live at a different path (the shared `LICENSE` and the reference
/// voice). All ten land as individual files directly inside
/// [`TTS_DIR_NAME`].
pub const TTS_FILES: &[FileSpec] = &[
    FileSpec {
        file_name: "bundle.json",
        sha256: "bab643150f437f37df080a710520ff39ed9ebd9a339f8ebdc739f7eddfc28b3f",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/bundle.json",
    },
    FileSpec {
        file_name: "bos_before_voice.npy",
        sha256: "f46edf4f7007b7ba4ea58831f49d003e59e167b4641c44bb3addfe9231a780b1",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/bos_before_voice.npy",
    },
    FileSpec {
        file_name: "tokenizer.model",
        sha256: "d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/tokenizer.model",
    },
    FileSpec {
        file_name: "flow_lm_main_int8.onnx",
        sha256: "f9bd8106b79a0192c1c43399ab938fb24900a95c1c599870d75a884e99000116",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/flow_lm_main_int8.onnx",
    },
    FileSpec {
        file_name: "flow_lm_flow_int8.onnx",
        sha256: "3dd781ee5abee9e195320bf0106bebd6372a852b3b36352524ee78b40554635d",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/flow_lm_flow_int8.onnx",
    },
    FileSpec {
        file_name: "mimi_decoder_int8.onnx",
        sha256: "3630450a3297a101792a6ac66619ebc70ab916b265e6220c2afaef8b1673f925",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/mimi_decoder_int8.onnx",
    },
    FileSpec {
        file_name: "mimi_encoder.onnx",
        sha256: "853e2ca623b8782d94c3745ec6133bfdff7ce33d9b11128bd29ea03f28d76e3d",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/mimi_encoder.onnx",
    },
    FileSpec {
        file_name: "text_conditioner.onnx",
        sha256: "4ecee995fb69f85c7a7493d11f7b5ee15d9950facc7ab3f5c9c49ef1e03847bb",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/english_2026-04/text_conditioner.onnx",
    },
    FileSpec {
        file_name: "LICENSE",
        sha256: "fe7b4ce83b8381cc5b216bbb4af73c570688d1b819c73bbaed8ca401f4677cd6",
        url: "https://huggingface.co/KevinAHM/pocket-tts-onnx/resolve/58a6d00cf13d239b6748cb0769f35c580a8f606c/onnx/LICENSE",
    },
    FileSpec {
        file_name: "reference_sample.wav",
        sha256: "a35b0468382218e9f37a9a7494d1e4b74deaf18d7ced22265b4e325bb55c183f",
        url: "https://huggingface.co/kyutai/tts-voices/resolve/323332d33f997de8394f24a193e1a76df720e01a/vctk/p333_023_enhanced.wav",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_urls_are_well_formed() {
        for f in TTS_FILES {
            assert!(
                f.url.starts_with("https://"),
                "{} has a non-https URL: {}",
                f.file_name,
                f.url
            );
            assert_eq!(
                f.sha256.len(),
                64,
                "{} sha256 is not 64 hex chars",
                f.file_name
            );
        }
    }

    #[test]
    fn stt_archive_sha256_is_64_hex_chars() {
        assert_eq!(STT_ARCHIVE_SHA256.len(), 64);
    }

    #[test]
    fn no_duplicate_tts_file_names() {
        let mut names: Vec<&str> = TTS_FILES.iter().map(|f| f.file_name).collect();
        names.sort_unstable();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped, "duplicate file_name in TTS_FILES");
    }

    #[test]
    fn model_license_text_matches_embedded_asset_on_disk() {
        // Cheap sanity check that the include_str! picked up the file we
        // think it did -- catches an empty/truncated asset at test time
        // rather than silently shipping a blank license notice.
        assert!(STT_MODEL_LICENSE_TEXT.contains("CC-BY-4.0"));
        assert!(STT_MODEL_LICENSE_TEXT.contains("NVIDIA"));
    }
}
