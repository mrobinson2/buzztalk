//! Model-dependent integration test. Requires the real Pocket TTS bundle at
//! `default_model_dir()` (or `BUZZTALK_TTS_MODEL_DIR`), so it is `#[ignore]`d
//! and must be run explicitly:
//!
//! ```sh
//! cargo test -p buzztalk-tts -- --ignored --nocapture
//! ```

use std::time::Instant;

use buzztalk_tts::{default_model_dir, PocketSynthesizer, SpeechSynthesizer};

#[test]
#[ignore = "requires the real Pocket TTS model bundle on disk"]
fn synthesizes_real_audio_and_reports_cold_vs_warm_timing() {
    let model_dir = default_model_dir();
    assert!(
        model_dir.is_dir(),
        "model directory not found at {}; set BUZZTALK_TTS_MODEL_DIR",
        model_dir.display()
    );

    let load_start = Instant::now();
    let mut synth = PocketSynthesizer::load(&model_dir, 1).expect("load Pocket TTS engine");
    println!("session load time: {:?}", load_start.elapsed());

    // Cold: the very first inference on freshly loaded ONNX sessions, no
    // warmup() call first. This is the number that decides whether a user's
    // first sentence sounds slow.
    let cold_start = Instant::now();
    let cold_chunk = synth
        .synthesize("Hello. This is BuzzTalk speaking.")
        .expect("cold synthesis");
    let cold_elapsed = cold_start.elapsed();

    // Warm: same engine, same voice, a second call right after. Sessions
    // have now run at least once each.
    let warm_start = Instant::now();
    let warm_chunk = synth
        .synthesize("Hello. This is BuzzTalk speaking.")
        .expect("warm synthesis");
    let warm_elapsed = warm_start.elapsed();

    assert!(
        !cold_chunk.samples.is_empty(),
        "cold synthesis produced no audio"
    );
    assert!(
        !warm_chunk.samples.is_empty(),
        "warm synthesis produced no audio"
    );
    assert_eq!(cold_chunk.sample_rate, 24_000);
    assert_eq!(warm_chunk.sample_rate, 24_000);
    assert!(cold_chunk.samples.iter().all(|s| s.is_finite()));
    assert!(
        cold_chunk.samples.iter().any(|s| s.abs() > 1.0e-6),
        "cold audio is silent"
    );
    assert!(
        warm_chunk.samples.iter().any(|s| s.abs() > 1.0e-6),
        "warm audio is silent"
    );
    assert_eq!(cold_chunk.chunk_index, 0);
    assert_eq!(warm_chunk.chunk_index, 1);

    println!(
        "COLD synthesis: {} samples ({:.2}s of audio) in {:?}",
        cold_chunk.samples.len(),
        cold_chunk.samples.len() as f32 / cold_chunk.sample_rate as f32,
        cold_elapsed
    );
    println!(
        "WARM synthesis: {} samples ({:.2}s of audio) in {:?}",
        warm_chunk.samples.len(),
        warm_chunk.samples.len() as f32 / warm_chunk.sample_rate as f32,
        warm_elapsed
    );

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: cold_chunk.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create("/tmp/buzztalk_tts_test.wav", spec).expect("create output wav");
    for sample in &cold_chunk.samples {
        let clamped = sample.clamp(-1.0, 1.0);
        writer
            .write_sample((clamped * i16::MAX as f32) as i16)
            .expect("write sample");
    }
    writer.finalize().expect("finalize wav");
    println!("wrote /tmp/buzztalk_tts_test.wav");
}

#[test]
#[ignore = "requires the real Pocket TTS model bundle on disk"]
fn warmup_produces_a_throwaway_synthesis_without_affecting_chunk_index() {
    let model_dir = default_model_dir();
    let mut synth = PocketSynthesizer::load(&model_dir, 1).expect("load Pocket TTS engine");

    let warmup_start = Instant::now();
    synth.warmup().expect("warmup");
    let warmup_elapsed = warmup_start.elapsed();
    println!("warmup time: {warmup_elapsed:?}");

    let after_warmup_start = Instant::now();
    let chunk = synth
        .synthesize("Hello. This is BuzzTalk speaking.")
        .expect("post-warmup synthesis");
    let after_warmup_elapsed = after_warmup_start.elapsed();
    println!("post-warmup synthesis time: {after_warmup_elapsed:?}");

    assert_eq!(
        chunk.chunk_index, 0,
        "warmup must not consume the caller's chunk index sequence"
    );
    assert!(!chunk.samples.is_empty());
    assert!(
        after_warmup_elapsed < warmup_elapsed,
        "expected post-warmup synthesis ({after_warmup_elapsed:?}) to be faster than warmup itself ({warmup_elapsed:?})"
    );
}
