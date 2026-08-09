//! The Pocket TTS inference pipeline: five ONNX sessions (voice encoder,
//! text conditioner, recurrent flow-LM, flow-matching decoder, Mimi audio
//! decoder) wired together exactly as the `english_2026-04` bundle expects.
//!
//! Ported from Block's `buzz-voice` crate (`src/pocket_april.rs` and
//! `src/pocket.rs`, Apache-2.0) — see the crate-level attribution in
//! `lib.rs`. Adapted here to this crate's [`crate::Error`] instead of
//! `String`, and to load from a `PathBuf` model directory resolved by
//! [`crate::model_dir`].

use std::borrow::Cow;
use std::f32::consts::TAU;
use std::path::Path;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use rand::{Rng, RngExt};
use tokenizers::Tokenizer;

use crate::bundle::{self, Bundle};
use crate::error::{Error, Result};
use crate::npy::read_npy_f32;
use crate::state::{append_state_inputs, initialize_state, replace_state_from_outputs, StateValue};
use crate::tokenizer::load_tokenizer;
use crate::wav::VoiceStyle;

const DEFAULT_TEMPERATURE: f32 = 0.7;
const EOS_LOGIT_THRESHOLD: f32 = -4.0;
const DECODER_CHUNK_FRAMES: usize = 12;
const TOKENS_PER_SECOND_ESTIMATE: f32 = 3.0;
const GENERATION_SECONDS_PADDING: f32 = 2.0;

/// Bundle's own hard limit on how many SentencePiece tokens a single
/// inference call may contain. Independent of, and much finer-grained
/// than, [`crate::segmentation`]'s latency-oriented phrase splitting: a
/// single phrase chunk can still exceed 50 tokens and needs a second,
/// token-budget-aware split before it reaches the model.
pub const MAX_TOKENS_PER_INFERENCE: usize = 50;

fn onnx_error<R>(context: impl Into<String>) -> impl FnOnce(ort::Error<R>) -> Error
where
    ort::Error<R>: Into<ort::Error>,
{
    let context = context.into();
    move |source| Error::Onnx {
        context,
        source: source.into(),
    }
}

/// Text cleaned up and annotated the way the bundle expects: capitalized,
/// terminated, with a heuristic for how many extra frames to generate past
/// the model's own end-of-speech signal.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedPrompt {
    pub text: String,
    pub frames_after_eos: usize,
}

/// Normalize whitespace, capitalize the first letter, ensure terminal
/// punctuation, and estimate how many frames of padding to generate after
/// the model signals end-of-speech.
pub fn prepare_prompt(input: &str) -> Option<PreparedPrompt> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut cleaned = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                cleaned.push(' ');
            }
            last_was_space = true;
        } else {
            cleaned.push(ch);
            last_was_space = false;
        }
    }

    let first = cleaned.chars().next().expect("cleaned non-empty above");
    if first.is_lowercase() {
        let upper: String = first.to_uppercase().collect();
        let mut iter = cleaned.chars();
        iter.next();
        cleaned = upper + iter.as_str();
    }

    let last = cleaned
        .chars()
        .next_back()
        .expect("cleaned non-empty above");
    if last.is_alphanumeric() {
        cleaned.push('.');
    }

    let word_count = cleaned.split_whitespace().count();
    Some(PreparedPrompt {
        text: cleaned,
        // Mirrors the bundle's upstream heuristic: three generated frames
        // plus two trailing frames for short prompts, one plus two
        // otherwise.
        frames_after_eos: if word_count <= 4 { 5 } else { 3 },
    })
}

struct CachedVoice {
    samples_ptr: usize,
    samples_len: usize,
    sample_rate: u32,
    embeddings: Vec<f32>,
}

/// Resident Pocket TTS engine: five loaded ONNX sessions plus the bundle
/// manifest, tokenizer, and voice-BOS embedding needed to drive them.
pub struct PocketEngine {
    bundle: Bundle,
    tokenizer: Tokenizer,
    bos_embedding: Vec<f32>,
    mimi_encoder: Session,
    text_conditioner: Session,
    flow_main: Session,
    flow: Session,
    mimi_decoder: Session,
    cached_voice: Option<CachedVoice>,
}

impl PocketEngine {
    /// Load every session and asset from `dir`, which must already have
    /// passed [`bundle::check_required_files`].
    pub fn load(dir: &Path, num_threads: usize) -> Result<Self> {
        bundle::check_required_files(dir)?;
        let bundle = bundle::load(dir)?;

        let tokenizer_path = dir.join(&bundle.tokenizer_file);
        let tokenizer = load_tokenizer(&tokenizer_path)?;

        let bos_path = dir.join(&bundle.bos_before_voice_file);
        let bos_embedding = read_npy_f32(&bos_path)?;
        if bos_embedding.len() != bundle.conditioning_dim {
            return Err(Error::UnsupportedBundle(format!(
                "{} has {} values; expected {}",
                bos_path.display(),
                bos_embedding.len(),
                bundle.conditioning_dim
            )));
        }

        Ok(Self {
            // The INT8 layout quantizes only the three generation graphs;
            // voice encoding and text conditioning remain full precision.
            mimi_encoder: load_session(&dir.join(bundle::FILE_MIMI_ENCODER), num_threads)?,
            text_conditioner: load_session(&dir.join(bundle::FILE_TEXT_CONDITIONER), num_threads)?,
            flow_main: load_session(&dir.join(bundle::FILE_FLOW_MAIN_INT8), num_threads)?,
            flow: load_session(&dir.join(bundle::FILE_FLOW_INT8), num_threads)?,
            mimi_decoder: load_session(&dir.join(bundle::FILE_MIMI_DECODER_INT8), num_threads)?,
            bundle,
            tokenizer,
            bos_embedding,
            cached_voice: None,
        })
    }

    /// The bundle's audio output sample rate (24 kHz for this bundle).
    pub fn sample_rate(&self) -> u32 {
        self.bundle.sample_rate as u32
    }

    /// Split already-[`prepare_prompt`]-ed text into chunks that each fit
    /// within [`MAX_TOKENS_PER_INFERENCE`] SentencePiece tokens. A no-op
    /// for the common case of short phrases.
    pub fn split_prompt(&self, prepared: &PreparedPrompt) -> Result<Vec<String>> {
        if self.token_count(&prepared.text)? <= MAX_TOKENS_PER_INFERENCE {
            return Ok(vec![prepared.text.clone()]);
        }

        let mut chunks = Vec::new();
        let mut current = String::new();
        for word in prepared.text.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if self.prepared_token_count(&candidate)? <= MAX_TOKENS_PER_INFERENCE {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            if self.prepared_token_count(word)? <= MAX_TOKENS_PER_INFERENCE {
                current = word.to_string();
                continue;
            }
            let mut fragment = String::new();
            for ch in word.chars() {
                let candidate = format!("{fragment}{ch}");
                if !fragment.is_empty()
                    && self.prepared_token_count(&candidate)? > MAX_TOKENS_PER_INFERENCE
                {
                    chunks.push(std::mem::take(&mut fragment));
                }
                fragment.push(ch);
            }
            current = fragment;
        }
        if !current.is_empty() {
            chunks.push(current);
        }

        chunks
            .into_iter()
            .map(|text| {
                let chunk = prepare_prompt(&text)
                    .ok_or_else(|| Error::Synthesis("prompt chunk became empty".to_string()))?;
                let token_count = self.token_count(&chunk.text)?;
                if token_count > MAX_TOKENS_PER_INFERENCE {
                    return Err(Error::Synthesis(format!(
                        "prompt chunk has {token_count} tokens; maximum is {MAX_TOKENS_PER_INFERENCE}"
                    )));
                }
                Ok(chunk.text)
            })
            .collect()
    }

    /// Synthesize `text` end to end: prepare it, split it to respect the
    /// bundle's token budget if necessary, and concatenate the resulting
    /// audio for every sub-chunk.
    pub fn synthesize_text(&mut self, text: &str, style: &VoiceStyle) -> Result<Vec<f32>> {
        let Some(prepared) = prepare_prompt(text) else {
            return Ok(Vec::new());
        };
        let chunks = self.split_prompt(&prepared)?;
        let mut samples = Vec::new();
        for chunk in chunks {
            let prepared = prepare_prompt(&chunk)
                .ok_or_else(|| Error::Synthesis("prompt chunk became empty".to_string()))?;
            samples.extend(self.synth_chunk(&prepared, style)?);
        }
        Ok(samples)
    }

    fn synth_chunk(&mut self, prepared: &PreparedPrompt, style: &VoiceStyle) -> Result<Vec<f32>> {
        let voice_embeddings = self.voice_embeddings(style)?;
        let mut flow_state = self.condition_voice(&voice_embeddings)?;
        let token_ids = self
            .tokenizer
            .encode(prepared.text.as_str(), false)
            .map_err(|err| Error::Tokenizer(format!("tokenizing prompt: {err}")))?
            .get_ids()
            .iter()
            .copied()
            .map(i64::from)
            .collect::<Vec<_>>();
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }
        if token_ids.len() > MAX_TOKENS_PER_INFERENCE {
            return Err(Error::Synthesis(format!(
                "prompt has {} tokens; split_prompt maximum is {MAX_TOKENS_PER_INFERENCE}",
                token_ids.len()
            )));
        }

        let token_count = token_ids.len();
        let text_embeddings = self.text_embeddings(token_ids)?;
        self.run_flow_main_prefix(&text_embeddings, &mut flow_state)?;
        let max_frames = estimate_max_frames(token_count, self.bundle.frame_rate);
        let latents =
            self.generate_latents(max_frames, prepared.frames_after_eos, &mut flow_state)?;
        self.decode_latents(&latents)
    }

    fn prepared_token_count(&self, text: &str) -> Result<usize> {
        let prepared = prepare_prompt(text)
            .ok_or_else(|| Error::Synthesis("prompt chunk became empty".to_string()))?;
        self.token_count(&prepared.text)
    }

    fn token_count(&self, text: &str) -> Result<usize> {
        Ok(self
            .tokenizer
            .encode(text, false)
            .map_err(|err| Error::Tokenizer(format!("tokenizing prompt: {err}")))?
            .get_ids()
            .len())
    }

    fn voice_embeddings(&mut self, style: &VoiceStyle) -> Result<Vec<f32>> {
        let key = (
            style.samples.as_ptr() as usize,
            style.samples.len(),
            style.sample_rate,
        );
        if let Some(cached) = &self.cached_voice {
            if (cached.samples_ptr, cached.samples_len, cached.sample_rate) == key {
                return Ok(cached.embeddings.clone());
            }
        }

        let samples = if style.sample_rate == self.bundle.sample_rate as u32 {
            style.samples.clone()
        } else {
            crate::wav::resample_linear(
                &style.samples,
                style.sample_rate,
                self.bundle.sample_rate as u32,
            )?
        };
        let audio = Tensor::from_array((
            vec![1_i64, 1, samples.len() as i64],
            samples.into_boxed_slice(),
        ))
        .map_err(onnx_error("creating the voice audio tensor"))?;
        let outputs = self
            .mimi_encoder
            .run(ort::inputs!["audio" => audio])
            .map_err(onnx_error("running the Mimi encoder"))?;
        let (_, encoded) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(onnx_error("extracting the Mimi encoder output"))?;
        if !encoded.len().is_multiple_of(self.bundle.conditioning_dim) {
            return Err(Error::Synthesis(format!(
                "Mimi encoder returned {} values, not divisible by {}",
                encoded.len(),
                self.bundle.conditioning_dim
            )));
        }
        let mut embeddings =
            Vec::with_capacity(self.bos_embedding.len().saturating_add(encoded.len()));
        embeddings.extend_from_slice(&self.bos_embedding);
        embeddings.extend_from_slice(encoded);
        self.cached_voice = Some(CachedVoice {
            samples_ptr: key.0,
            samples_len: key.1,
            sample_rate: key.2,
            embeddings: embeddings.clone(),
        });
        Ok(embeddings)
    }

    fn condition_voice(&mut self, embeddings: &[f32]) -> Result<Vec<StateValue>> {
        let frames = embeddings.len() / self.bundle.conditioning_dim;
        let sequence = Tensor::<f32>::new(
            &ort::memory::Allocator::default(),
            [1_i64, 0, self.bundle.latent_dim as i64],
        )
        .map_err(onnx_error("creating an empty voice sequence"))?;
        let text_embeddings = Tensor::from_array((
            vec![1_i64, frames as i64, self.bundle.conditioning_dim as i64],
            embeddings.to_vec().into_boxed_slice(),
        ))
        .map_err(onnx_error("creating the voice embedding tensor"))?;
        let mut state = initialize_state(&self.bundle.flow_lm_state_manifest)?;
        let mut inputs = vec![
            (Cow::Borrowed("sequence"), SessionInputValue::from(sequence)),
            (
                Cow::Borrowed("text_embeddings"),
                SessionInputValue::from(text_embeddings),
            ),
        ];
        append_state_inputs(&mut inputs, &state);
        let mut outputs = self
            .flow_main
            .run(inputs)
            .map_err(onnx_error("conditioning the voice"))?;
        replace_state_from_outputs(&mut state, &mut outputs)?;
        Ok(state)
    }

    fn text_embeddings(&mut self, token_ids: Vec<i64>) -> Result<Vec<f32>> {
        let tokens = Tensor::from_array((
            vec![1_i64, token_ids.len() as i64],
            token_ids.into_boxed_slice(),
        ))
        .map_err(onnx_error("creating the token tensor"))?;
        let outputs = self
            .text_conditioner
            .run(ort::inputs!["token_ids" => tokens])
            .map_err(onnx_error("running the text conditioner"))?;
        let (_, embeddings) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(onnx_error("extracting text embeddings"))?;
        Ok(embeddings.to_vec())
    }

    fn run_flow_main_prefix(
        &mut self,
        text_embeddings: &[f32],
        state: &mut [StateValue],
    ) -> Result<()> {
        if !text_embeddings
            .len()
            .is_multiple_of(self.bundle.conditioning_dim)
        {
            return Err(Error::Synthesis(format!(
                "text conditioner returned {} values, not divisible by {}",
                text_embeddings.len(),
                self.bundle.conditioning_dim
            )));
        }
        let frames = text_embeddings.len() / self.bundle.conditioning_dim;
        let sequence = Tensor::<f32>::new(
            &ort::memory::Allocator::default(),
            [1_i64, 0, self.bundle.latent_dim as i64],
        )
        .map_err(onnx_error("creating an empty text sequence"))?;
        let text_embeddings = Tensor::from_array((
            vec![1_i64, frames as i64, self.bundle.conditioning_dim as i64],
            text_embeddings.to_vec().into_boxed_slice(),
        ))
        .map_err(onnx_error("creating the text embedding tensor"))?;
        let mut inputs = vec![
            (Cow::Borrowed("sequence"), SessionInputValue::from(sequence)),
            (
                Cow::Borrowed("text_embeddings"),
                SessionInputValue::from(text_embeddings),
            ),
        ];
        append_state_inputs(&mut inputs, state);
        let mut outputs = self
            .flow_main
            .run(inputs)
            .map_err(onnx_error("priming the text state"))?;
        replace_state_from_outputs(state, &mut outputs)
    }

    fn generate_latents(
        &mut self,
        max_frames: usize,
        frames_after_eos: usize,
        state: &mut [StateValue],
    ) -> Result<Vec<f32>> {
        let mut current = vec![f32::NAN; self.bundle.latent_dim];
        let mut latents = Vec::with_capacity(max_frames * self.bundle.latent_dim);
        let mut eos_step = None;
        let mut rng = rand::rng();

        for step in 0..max_frames {
            let sequence = Tensor::from_array((
                vec![1_i64, 1, self.bundle.latent_dim as i64],
                current.clone().into_boxed_slice(),
            ))
            .map_err(onnx_error("creating the latent input"))?;
            let text_embeddings = Tensor::<f32>::new(
                &ort::memory::Allocator::default(),
                [1_i64, 0, self.bundle.conditioning_dim as i64],
            )
            .map_err(onnx_error("creating an empty text input"))?;
            let mut inputs = vec![
                (Cow::Borrowed("sequence"), SessionInputValue::from(sequence)),
                (
                    Cow::Borrowed("text_embeddings"),
                    SessionInputValue::from(text_embeddings),
                ),
            ];
            append_state_inputs(&mut inputs, state);
            let mut outputs = self
                .flow_main
                .run(inputs)
                .map_err(onnx_error("running the Flow LM"))?;
            let conditioning = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(onnx_error("extracting Flow LM conditioning"))?
                .1
                .to_vec();
            let eos_logit = outputs[1]
                .try_extract_tensor::<f32>()
                .map_err(onnx_error("extracting Flow LM EOS logit"))?
                .1
                .first()
                .copied()
                .ok_or_else(|| {
                    Error::Synthesis("Flow LM returned an empty EOS logit".to_string())
                })?;
            replace_state_from_outputs(state, &mut outputs)?;

            if eos_logit > EOS_LOGIT_THRESHOLD && eos_step.is_none() {
                eos_step = Some(step);
            }
            if eos_step.is_some_and(|eos| step >= eos + frames_after_eos) {
                break;
            }

            let mut noise =
                normal_noise(&mut rng, self.bundle.latent_dim, DEFAULT_TEMPERATURE.sqrt());
            let conditioning = Tensor::from_array((
                vec![1_i64, self.bundle.conditioning_dim as i64],
                conditioning.into_boxed_slice(),
            ))
            .map_err(onnx_error("creating the flow conditioning tensor"))?;
            let s = Tensor::from_array((vec![1_i64, 1], vec![0.0_f32].into_boxed_slice()))
                .map_err(onnx_error("creating the flow start tensor"))?;
            let t = Tensor::from_array((vec![1_i64, 1], vec![1.0_f32].into_boxed_slice()))
                .map_err(onnx_error("creating the flow end tensor"))?;
            let x = Tensor::from_array((
                vec![1_i64, self.bundle.latent_dim as i64],
                noise.clone().into_boxed_slice(),
            ))
            .map_err(onnx_error("creating the flow noise tensor"))?;
            let outputs = self
                .flow
                .run(ort::inputs!["c" => conditioning, "s" => s, "t" => t, "x" => x])
                .map_err(onnx_error("running the flow"))?;
            let flow = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(onnx_error("extracting the flow output"))?
                .1;
            if flow.len() != noise.len() {
                return Err(Error::Synthesis(format!(
                    "flow returned {} values; expected {}",
                    flow.len(),
                    noise.len()
                )));
            }
            for (sample, delta) in noise.iter_mut().zip(flow) {
                *sample += *delta;
            }
            current.clone_from(&noise);
            latents.extend_from_slice(&noise);
        }
        Ok(latents)
    }

    fn decode_latents(&mut self, latents: &[f32]) -> Result<Vec<f32>> {
        if latents.is_empty() {
            return Ok(Vec::new());
        }
        if !latents.len().is_multiple_of(self.bundle.latent_dim) {
            return Err(Error::Synthesis(format!(
                "latent buffer has {} values, not divisible by {}",
                latents.len(),
                self.bundle.latent_dim
            )));
        }
        let frame_count = latents.len() / self.bundle.latent_dim;
        let mut state = initialize_state(&self.bundle.mimi_state_manifest)?;
        let mut audio = Vec::new();

        for start in (0..frame_count).step_by(DECODER_CHUNK_FRAMES) {
            let end = (start + DECODER_CHUNK_FRAMES).min(frame_count);
            let values =
                latents[start * self.bundle.latent_dim..end * self.bundle.latent_dim].to_vec();
            let latent = Tensor::from_array((
                vec![1_i64, (end - start) as i64, self.bundle.latent_dim as i64],
                values.into_boxed_slice(),
            ))
            .map_err(onnx_error("creating the Mimi latent tensor"))?;
            let mut inputs = vec![(Cow::Borrowed("latent"), SessionInputValue::from(latent))];
            append_state_inputs(&mut inputs, &state);
            let mut outputs = self
                .mimi_decoder
                .run(inputs)
                .map_err(onnx_error("running the Mimi decoder"))?;
            let samples = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(onnx_error("extracting Mimi audio"))?
                .1;
            audio.extend_from_slice(samples);
            replace_state_from_outputs(&mut state, &mut outputs)?;
        }
        Ok(audio)
    }
}

fn load_session(path: &Path, num_threads: usize) -> Result<Session> {
    if !path.is_file() {
        return Err(Error::MissingFile {
            file: "onnx model file",
            path: path.to_path_buf(),
        });
    }
    Session::builder()
        .map_err(onnx_error("creating the ONNX session builder"))?
        .with_intra_threads(num_threads)
        .map_err(onnx_error("configuring intra-op threads"))?
        .with_inter_threads(1)
        .map_err(onnx_error("configuring inter-op threads"))?
        .commit_from_file(path)
        .map_err(onnx_error(format!("loading {}", path.display())))
}

fn estimate_max_frames(token_count: usize, frame_rate: f32) -> usize {
    ((token_count as f32 / TOKENS_PER_SECOND_ESTIMATE + GENERATION_SECONDS_PADDING) * frame_rate)
        .ceil() as usize
}

fn normal_noise(rng: &mut impl Rng, len: usize, std_dev: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let u1 = rng.random::<f32>().max(f32::MIN_POSITIVE);
        let u2 = rng.random::<f32>();
        let radius = (-2.0_f32 * u1.ln()).sqrt() * std_dev;
        out.push(radius * (TAU * u2).cos());
        if out.len() < len {
            out.push(radius * (TAU * u2).sin());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_prompt_capitalizes_and_terminates() {
        let prepared = prepare_prompt("hello there").unwrap();
        assert_eq!(prepared.text, "Hello there.");
    }

    #[test]
    fn prepare_prompt_collapses_whitespace() {
        let prepared = prepare_prompt("  hi   there  \n\n friend  ").unwrap();
        assert_eq!(prepared.text, "Hi there friend.");
    }

    #[test]
    fn prepare_prompt_rejects_empty_input() {
        assert!(prepare_prompt("").is_none());
        assert!(prepare_prompt("   ").is_none());
    }

    #[test]
    fn prepare_prompt_short_inputs_get_more_trailing_frames() {
        assert_eq!(prepare_prompt("Hi.").unwrap().frames_after_eos, 5);
        assert_eq!(
            prepare_prompt("This is a much longer prompt with several words in it.")
                .unwrap()
                .frames_after_eos,
            3
        );
    }

    #[test]
    fn estimate_max_frames_scales_with_token_count() {
        assert_eq!(estimate_max_frames(3, 12.5), 38);
        assert_eq!(estimate_max_frames(300, 12.5), 1_275);
    }

    #[test]
    fn normal_noise_has_requested_length() {
        let mut rng = rand::rng();
        assert_eq!(normal_noise(&mut rng, 1, 1.0).len(), 1);
        assert_eq!(normal_noise(&mut rng, 32, 1.0).len(), 32);
    }
}
