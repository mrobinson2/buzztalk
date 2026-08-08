//! SentencePiece tokenizer loading for the Pocket TTS bundle.
//!
//! The bundle ships a raw SentencePiece `tokenizer.model` protobuf, not a
//! `tokenizers`-native JSON file, so it is converted into a `tokenizers`
//! `Unigram` model at load time.
//!
//! Ported from Block's `buzz-voice` crate (`src/pocket_april.rs`,
//! Apache-2.0) — see the crate-level attribution in `lib.rs`.

use std::path::Path;

use sentencepiece_model::SentencePieceModel;
use tokenizers::models::unigram::Unigram;
use tokenizers::pre_tokenizers::metaspace::{Metaspace, PrependScheme};
use tokenizers::Tokenizer;

use crate::error::{Error, Result};

/// Load and convert the bundle's `tokenizer.model` SentencePiece file.
pub fn load_tokenizer(path: &Path) -> Result<Tokenizer> {
    let tok_err = |msg: String| Error::Tokenizer(format!("{}: {msg}", path.display()));

    let sentencepiece =
        SentencePieceModel::from_file(path).map_err(|err| tok_err(format!("{err}")))?;
    let trainer = sentencepiece
        .trainer()
        .ok_or_else(|| tok_err("no SentencePiece trainer metadata".to_string()))?;
    let normalizer = sentencepiece
        .normalizer()
        .ok_or_else(|| tok_err("no SentencePiece normalizer metadata".to_string()))?;
    if normalizer.name() != "identity" {
        return Err(tok_err(format!(
            "unsupported SentencePiece normalizer {:?}",
            normalizer.name()
        )));
    }

    let vocab = sentencepiece
        .pieces()
        .iter()
        .map(|piece| (piece.piece().to_owned(), f64::from(piece.score())))
        .collect();
    let mut tokenizer = Tokenizer::new(
        Unigram::from(
            vocab,
            Some(trainer.unk_id() as usize),
            trainer.byte_fallback(),
        )
        .map_err(|err| tok_err(format!("constructing unigram model: {err}")))?,
    );
    // SentencePiece's identity normalizer still escapes spaces as U+2581 and
    // prepends one marker to the input before unigram segmentation.
    tokenizer.with_pre_tokenizer(Some(Metaspace::new(
        '\u{2581}',
        PrependScheme::Always,
        false,
    )));
    Ok(tokenizer)
}
