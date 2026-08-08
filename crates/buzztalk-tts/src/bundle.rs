//! `bundle.json` manifest parsing and validation for the Pocket TTS
//! `english_2026-04` bundle.
//!
//! Ported from Block's `buzz-voice` crate (`src/pocket_april.rs`,
//! Apache-2.0) — see the crate-level attribution in `lib.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Bundle-relative file names this crate knows how to load.
pub const FILE_BUNDLE: &str = "bundle.json";
/// Voice/reference-audio encoder. Full precision in the INT8 variant.
pub const FILE_MIMI_ENCODER: &str = "mimi_encoder.onnx";
/// Text embedding model. Full precision in the INT8 variant.
pub const FILE_TEXT_CONDITIONER: &str = "text_conditioner.onnx";
/// Recurrent Flow LM "main" graph (quantized).
pub const FILE_FLOW_MAIN_INT8: &str = "flow_lm_main_int8.onnx";
/// Flow-matching decoder graph (quantized).
pub const FILE_FLOW_INT8: &str = "flow_lm_flow_int8.onnx";
/// Mimi audio decoder (quantized).
pub const FILE_MIMI_DECODER_INT8: &str = "mimi_decoder_int8.onnx";
/// Bundled reference voice WAV.
pub const FILE_REFERENCE_VOICE: &str = "reference_sample.wav";

const MODEL_LANGUAGE: &str = "english_2026-04";
const EXPECTED_SCHEMA_VERSION: u32 = 2;

/// Every file [`load`] and the engine require to be present in the model
/// directory, beyond what `bundle.json` itself names dynamically
/// (`tokenizer_file`, `bos_before_voice_file`).
pub const REQUIRED_FIXED_FILES: &[&str] = &[
    FILE_BUNDLE,
    FILE_MIMI_ENCODER,
    FILE_TEXT_CONDITIONER,
    FILE_FLOW_MAIN_INT8,
    FILE_FLOW_INT8,
    FILE_MIMI_DECODER_INT8,
    FILE_REFERENCE_VOICE,
];

/// Parsed and validated `bundle.json`.
#[derive(Debug, Deserialize)]
pub struct Bundle {
    pub schema_version: u32,
    pub language: String,
    pub sample_rate: usize,
    pub frame_rate: f32,
    pub samples_per_frame: usize,
    pub latent_dim: usize,
    pub conditioning_dim: usize,
    pub insert_bos_before_voice: bool,
    pub pad_with_spaces_for_short_inputs: bool,
    pub remove_semicolons: bool,
    pub model_recommended_frames_after_eos: Option<usize>,
    pub max_token_per_chunk: usize,
    pub tokenizer_file: String,
    pub bos_before_voice_file: String,
    pub flow_lm_state_manifest: Vec<StateSpec>,
    pub mimi_state_manifest: Vec<StateSpec>,
}

/// One recurrent state tensor an ONNX graph carries between calls.
#[derive(Debug, Clone, Deserialize)]
pub struct StateSpec {
    pub input_name: String,
    pub output_name: String,
    pub dtype: StateDtype,
    pub shape: Vec<i64>,
    pub fill: StateFill,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateDtype {
    #[serde(rename = "float32")]
    Float32,
    #[serde(rename = "int64")]
    Int64,
    Bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateFill {
    Empty,
    Nan,
    Ones,
    Zeros,
}

/// Verify every file this crate needs is present in `dir`, returning a
/// clear typed error naming the first one that's missing rather than
/// panicking or letting a downstream ONNX/tokenizer error stand in for it.
pub fn check_required_files(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Err(Error::ModelDirMissing(dir.to_path_buf()));
    }
    for file in REQUIRED_FIXED_FILES {
        let path = dir.join(file);
        if !path.is_file() {
            return Err(Error::MissingFile { file, path });
        }
    }
    Ok(())
}

/// Load and validate `bundle.json` from `dir`.
pub fn load(dir: &Path) -> Result<Bundle> {
    let path = dir.join(FILE_BUNDLE);
    let bytes = fs::read(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let bundle: Bundle = serde_json::from_slice(&bytes).map_err(|source| Error::Json {
        path: path.clone(),
        source,
    })?;
    validate(&bundle)?;

    // The dynamically-named files must exist too.
    for (file, name) in [
        (bundle.tokenizer_file.as_str(), "tokenizer_file"),
        (
            bundle.bos_before_voice_file.as_str(),
            "bos_before_voice_file",
        ),
    ] {
        let file_path = dir.join(file);
        if !file_path.is_file() {
            return Err(Error::UnsupportedBundle(format!(
                "bundle.json names `{name}` = \"{file}\" but that file does not exist at {}",
                file_path.display()
            )));
        }
    }

    Ok(bundle)
}

fn validate(bundle: &Bundle) -> Result<()> {
    if bundle.schema_version != EXPECTED_SCHEMA_VERSION {
        return Err(Error::UnsupportedBundle(format!(
            "bundle schema {} (expected {EXPECTED_SCHEMA_VERSION})",
            bundle.schema_version
        )));
    }
    if bundle.language != MODEL_LANGUAGE {
        return Err(Error::UnsupportedBundle(format!(
            "bundle language `{}` (expected `{MODEL_LANGUAGE}`)",
            bundle.language
        )));
    }
    if bundle.sample_rate != 24_000
        || bundle.frame_rate != 12.5
        || bundle.samples_per_frame != 1_920
        || bundle.latent_dim != 32
        || bundle.conditioning_dim != 1_024
    {
        return Err(Error::UnsupportedBundle(format!(
            "unexpected bundle dimensions: sample_rate={}, frame_rate={}, \
             samples_per_frame={}, latent_dim={}, conditioning_dim={}",
            bundle.sample_rate,
            bundle.frame_rate,
            bundle.samples_per_frame,
            bundle.latent_dim,
            bundle.conditioning_dim
        )));
    }
    if !bundle.insert_bos_before_voice {
        return Err(Error::UnsupportedBundle(
            "bundle must insert BOS before voice".to_string(),
        ));
    }
    if bundle.pad_with_spaces_for_short_inputs
        || bundle.remove_semicolons
        || bundle.model_recommended_frames_after_eos.is_some()
        || bundle.max_token_per_chunk != 50
    {
        return Err(Error::UnsupportedBundle(
            "unsupported prompt-policy metadata in bundle.json".to_string(),
        ));
    }
    Ok(())
}

/// Path to the bundled reference voice WAV.
pub fn reference_voice_path(dir: &Path) -> PathBuf {
    dir.join(FILE_REFERENCE_VOICE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 2,
            "language": "english_2026-04",
            "sample_rate": 24000,
            "frame_rate": 12.5,
            "samples_per_frame": 1920,
            "latent_dim": 32,
            "conditioning_dim": 1024,
            "insert_bos_before_voice": true,
            "pad_with_spaces_for_short_inputs": false,
            "remove_semicolons": false,
            "model_recommended_frames_after_eos": null,
            "max_token_per_chunk": 50,
            "tokenizer_file": "tokenizer.model",
            "bos_before_voice_file": "bos_before_voice.npy",
            "flow_lm_state_manifest": [],
            "mimi_state_manifest": []
        })
    }

    #[test]
    fn valid_bundle_passes_validation() {
        let bundle: Bundle = serde_json::from_value(sample_json()).unwrap();
        assert!(validate(&bundle).is_ok());
    }

    #[test]
    fn wrong_schema_version_is_rejected() {
        let mut json = sample_json();
        json["schema_version"] = serde_json::json!(1);
        let bundle: Bundle = serde_json::from_value(json).unwrap();
        assert!(matches!(
            validate(&bundle),
            Err(Error::UnsupportedBundle(_))
        ));
    }

    #[test]
    fn wrong_language_is_rejected() {
        let mut json = sample_json();
        json["language"] = serde_json::json!("french_2026-04");
        let bundle: Bundle = serde_json::from_value(json).unwrap();
        assert!(matches!(
            validate(&bundle),
            Err(Error::UnsupportedBundle(_))
        ));
    }

    #[test]
    fn wrong_dimensions_are_rejected() {
        let mut json = sample_json();
        json["latent_dim"] = serde_json::json!(64);
        let bundle: Bundle = serde_json::from_value(json).unwrap();
        assert!(matches!(
            validate(&bundle),
            Err(Error::UnsupportedBundle(_))
        ));
    }

    #[test]
    fn missing_bos_before_voice_is_rejected() {
        let mut json = sample_json();
        json["insert_bos_before_voice"] = serde_json::json!(false);
        let bundle: Bundle = serde_json::from_value(json).unwrap();
        assert!(matches!(
            validate(&bundle),
            Err(Error::UnsupportedBundle(_))
        ));
    }

    #[test]
    fn unexpected_prompt_policy_is_rejected() {
        let mut json = sample_json();
        json["max_token_per_chunk"] = serde_json::json!(80);
        let bundle: Bundle = serde_json::from_value(json).unwrap();
        assert!(matches!(
            validate(&bundle),
            Err(Error::UnsupportedBundle(_))
        ));
    }

    #[test]
    fn missing_model_dir_is_a_typed_error_not_a_panic() {
        let err = check_required_files(Path::new("/nonexistent/does/not/exist")).unwrap_err();
        assert!(matches!(err, Error::ModelDirMissing(_)));
    }

    #[test]
    fn missing_file_in_existing_dir_is_a_typed_error() {
        let temp = tempfile::tempdir().unwrap();
        let err = check_required_files(temp.path()).unwrap_err();
        assert!(matches!(err, Error::MissingFile { .. }));
    }
}
