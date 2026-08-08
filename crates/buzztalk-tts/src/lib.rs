//! Streaming text-to-speech for BuzzTalk, backed by Kyutai's Pocket TTS
//! (`english_2026-04`, INT8 three-graph variant).
//!
//! ## Attribution
//!
//! The ONNX inference pipeline in [`engine`], [`state`], [`bundle`],
//! [`npy`], and [`tokenizer`] is ported from Block's `buzz-voice` crate
//! (`src/pocket.rs` and `src/pocket_april.rs`), licensed Apache-2.0. The
//! resampler in [`wav::resample_linear`] is ported from the same crate's
//! `src/imported.rs`. `buzz-voice`'s own attribution for the bundle itself:
//!
//! - Pocket TTS and Mimi: Kyutai, CC-BY-4.0.
//! - ONNX export: KevinAHM/pocket-tts-onnx, CC-BY-4.0.
//!
//! See `buzz-voice`'s Apache-2.0 license text for the full grant.
//!
//! ## Why not `sherpa-onnx`
//!
//! `sherpa-onnx` 1.13 added native `OfflineTtsPocketModelConfig` support,
//! which looked like it might let this crate skip the ported engine
//! entirely. It does not work for this bundle: its C++ backend hard-requires
//! `--pocket-vocab-json` (a JSON vocab + token-scores export sherpa-onnx
//! produces from its own conversion pipeline), and the config struct has no
//! field at all for this bundle's `bundle.json` state manifest or
//! `bos_before_voice.npy`. This bundle ships a raw SentencePiece
//! `tokenizer.model` instead — a different upstream export
//! (`KevinAHM/pocket-tts-onnx`) than sherpa-onnx's own
//! `sherpa-onnx-pocket-tts-int8-*` bundles target. `OfflineTts::create`
//! fails immediately with that decisive error for our bundle layout, so the
//! engine here talks to `ort` directly instead, exactly as `buzz-voice`
//! does.
//!
//! ## Layout
//!
//! - [`segmentation`] — pure, model-free phrase splitting for low-latency
//!   streaming. Exhaustively unit tested; runs in CI without the model.
//! - [`model_dir`] — resolves where the bundle lives on disk.
//! - [`bundle`] — parses and validates `bundle.json`.
//! - [`engine`] — the five-ONNX-session inference pipeline.
//! - [`synthesizer`] — the public [`SpeechSynthesizer`] trait and its
//!   Pocket TTS implementation.
//!
//! Model-dependent tests are `#[ignore]`d; run them with
//! `cargo test -p buzztalk-tts -- --ignored`.

#![warn(missing_docs)]

mod bundle;
mod engine;
mod error;
mod model_dir;
mod npy;
mod segmentation;
mod state;
mod synthesizer;
mod tokenizer;
mod wav;

pub use error::{Error, Result};
pub use model_dir::{default_model_dir, MODEL_DIR_ENV_VAR};
pub use segmentation::{
    segment_into_phrases, FIRST_CHUNK_MAX_LEN, HARD_CAP_LEN, MIN_CLAUSE_SPLIT_LEN,
};
pub use synthesizer::{AudioChunk, PocketSynthesizer, SpeechSynthesizer};
