//! Regression test for the two-ONNX-Runtime segfault: `buzztalk-tts` and
//! `buzztalk-stt` both end up in the same binary once `buzztalk-pipeline`
//! links them together. If `buzztalk-tts`'s `ort` dependency ever links its
//! own ONNX Runtime again (e.g. by re-enabling `download-binaries`) instead
//! of binding to the one `sherpa-onnx` (used by `buzztalk-stt`, and pulled
//! into this crate purely as a runtime provider — see `Cargo.toml`) already
//! links, constructing both engines in the same process segfaults.
//!
//! Requires both the Pocket TTS bundle and the Parakeet STT model on disk,
//! so it is `#[ignore]`d.

use buzztalk_stt::ParakeetRecognizer;
use buzztalk_tts::{default_model_dir, PocketSynthesizer, SpeechSynthesizer};

fn have_stt_model() -> bool {
    buzztalk_stt::default_model_dir().is_dir()
}

fn have_tts_model() -> bool {
    default_model_dir().is_dir()
}

#[test]
#[ignore = "requires both the Pocket TTS and Parakeet STT models on disk"]
fn stt_then_tts_in_same_process() {
    assert!(
        have_stt_model(),
        "Parakeet model not found; see buzztalk_stt::default_model_dir()"
    );
    assert!(
        have_tts_model(),
        "Pocket TTS model not found; see buzztalk_tts::default_model_dir()"
    );

    // Construct the sherpa-onnx-backed recognizer FIRST, so its statically
    // linked ONNX Runtime is the one already resident in the process when
    // `ort` (buzztalk-tts's engine) initializes.
    let _recognizer =
        ParakeetRecognizer::with_default_model().expect("construct ParakeetRecognizer");

    let mut synth =
        PocketSynthesizer::load(&default_model_dir(), 1).expect("construct PocketSynthesizer");
    let chunk = synth
        .synthesize("Hello. This is BuzzTalk speaking.")
        .expect("synthesize after STT construction");

    assert!(!chunk.samples.is_empty());
    assert!(chunk.samples.iter().all(|s| s.is_finite()));
    assert!(chunk.samples.iter().any(|s| s.abs() > 1.0e-6));
    assert_eq!(chunk.sample_rate, 24_000);
}

#[test]
#[ignore = "requires both the Pocket TTS and Parakeet STT models on disk"]
fn tts_then_stt_in_same_process() {
    assert!(
        have_tts_model(),
        "Pocket TTS model not found; see buzztalk_tts::default_model_dir()"
    );
    assert!(
        have_stt_model(),
        "Parakeet model not found; see buzztalk_stt::default_model_dir()"
    );

    // Reverse order: construct the `ort`-backed synthesizer first, so if
    // `ort` ever links its own runtime again, this order is the one that
    // would crash on the SECOND construction instead of the first.
    let mut synth =
        PocketSynthesizer::load(&default_model_dir(), 1).expect("construct PocketSynthesizer");
    let chunk = synth
        .synthesize("Hello. This is BuzzTalk speaking.")
        .expect("synthesize before STT construction");

    assert!(!chunk.samples.is_empty());
    assert!(chunk.samples.iter().all(|s| s.is_finite()));
    assert!(chunk.samples.iter().any(|s| s.abs() > 1.0e-6));
    assert_eq!(chunk.sample_rate, 24_000);

    let _recognizer =
        ParakeetRecognizer::with_default_model().expect("construct ParakeetRecognizer");
}
