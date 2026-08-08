//! Synthetic, deterministic tests for both detectors.
//!
//! Every signal here is generated in closed form (sine tones, or all-zero
//! silence) -- no randomness, no fixtures, no hardware. Frequencies and
//! amplitudes were chosen and then verified against this crate's default
//! thresholds; see inline comments where a specific number matters.

use buzztalk_core::{
    AecStats, DetectorEvent, OutputRoute, SpeechDetector, FRAME_SAMPLES, SAMPLE_RATE_HZ,
};
use buzztalk_vad::{BargeInConfig, BargeInDetector, EndpointConfig, EndpointDetector};

type Frame = [f32; FRAME_SAMPLES];

fn silence(n: usize) -> Vec<Frame> {
    vec![[0.0; FRAME_SAMPLES]; n]
}

/// One frame of a pure tone, phase reset to zero at the start of the frame.
fn tone_frame(freq_hz: f32, amplitude: f32) -> Frame {
    let mut frame = [0.0_f32; FRAME_SAMPLES];
    for (i, s) in frame.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE_HZ as f32;
        *s = amplitude * (2.0 * std::f32::consts::PI * freq_hz * t).sin();
    }
    frame
}

/// A "speech-like" signal: frame-local tone bursts that step through
/// distinctly different frequencies each frame. Real speech's spectral
/// content moves around as formants and pitch change; this is a cheap stand
/// in for that movement, giving frame-to-frame zero-crossing-rate variance
/// well above the spectral-variation gate.
fn speech_like_frames(n: usize) -> Vec<Frame> {
    const FREQS_HZ: [f32; 4] = [200.0, 320.0, 260.0, 380.0];
    (0..n)
        .map(|i| tone_frame(FREQS_HZ[i % FREQS_HZ.len()], 0.4))
        .collect()
}

/// A synthetic cough: a *stationary* broadband burst (fixed sum of tones,
/// continuous phase across frames) rather than speech's frame-to-frame
/// movement. Its zero-crossing rate is nearly identical frame to frame,
/// which is exactly the "impulse, not speech" signature the
/// spectral-variation gate is meant to catch.
fn cough_frames(n_frames: usize) -> Vec<Frame> {
    const TONES: [(f32, f32); 4] = [(600.0, 0.15), (1300.0, 0.15), (2600.0, 0.1), (5200.0, 0.1)];
    let mut frames = Vec::with_capacity(n_frames);
    let mut sample_idx: u64 = 0;
    for _ in 0..n_frames {
        let mut frame = [0.0_f32; FRAME_SAMPLES];
        for s in frame.iter_mut() {
            let t = sample_idx as f32 / SAMPLE_RATE_HZ as f32;
            let mut v = 0.0_f32;
            for (freq, amp) in TONES {
                v += amp * (2.0 * std::f32::consts::PI * freq * t).sin();
            }
            *s = v;
            sample_idx += 1;
        }
        frames.push(frame);
        // continuous phase: don't reset sample_idx between frames.
    }
    frames
}

fn good_stats(erle_db: f32) -> AecStats {
    AecStats {
        erle_db: Some(erle_db),
        estimated_delay_ms: Some(40),
        double_talk: false,
    }
}

// ── EndpointDetector ─────────────────────────────────────────────────────

#[test]
fn endpoint_speech_start_and_end_at_expected_frame_counts() {
    let cfg = EndpointConfig::default();
    let mut det = EndpointDetector::new(cfg);

    let lead_in = 5;
    let speech_len = 80;
    let trail_silence = 40;

    let mut frames = Vec::new();
    frames.extend(silence(lead_in));
    frames.extend(speech_like_frames(speech_len));
    frames.extend(silence(trail_silence));

    let events: Vec<DetectorEvent> = frames.iter().map(|f| det.push_frame(f).unwrap()).collect();

    // Nothing happens during the silent lead-in.
    for e in &events[..lead_in] {
        assert_eq!(*e, DetectorEvent::Idle);
    }

    // SpeechStart fires on exactly the `min_voiced_frames`th consecutive
    // voiced frame -- interruption/turn-taking latency is a product
    // requirement, so this is asserted against the config value, not a
    // hardcoded frame number.
    let start_idx = lead_in + cfg.min_voiced_frames as usize - 1;
    for e in &events[lead_in..start_idx] {
        assert_eq!(
            *e,
            DetectorEvent::Idle,
            "should not fire before min_voiced_frames is reached"
        );
    }
    assert_eq!(events[start_idx], DetectorEvent::SpeechStart);

    // Every remaining voiced frame continues the utterance.
    let speech_end_idx = lead_in + speech_len;
    for e in &events[start_idx + 1..speech_end_idx] {
        assert_eq!(*e, DetectorEvent::SpeechContinue);
    }

    // Hangover: SpeechContinue for hangover_frames - 1 silent frames, then
    // SpeechEnd on exactly the hangover_frames-th.
    let end_idx = speech_end_idx + cfg.hangover_frames as usize - 1;
    for e in &events[speech_end_idx..end_idx] {
        assert_eq!(*e, DetectorEvent::SpeechContinue);
    }
    assert_eq!(events[end_idx], DetectorEvent::SpeechEnd);

    // And back to idle afterward.
    for e in &events[end_idx + 1..] {
        assert_eq!(*e, DetectorEvent::Idle);
    }
}

#[test]
fn endpoint_pure_silence_produces_no_detection_ever() {
    let mut det = EndpointDetector::default();
    for frame in silence(200) {
        assert_eq!(det.push_frame(&frame).unwrap(), DetectorEvent::Idle);
    }
}

#[test]
fn endpoint_short_blip_produces_no_speech_start() {
    let cfg = EndpointConfig::default();
    let mut det = EndpointDetector::new(cfg);

    let mut frames = Vec::new();
    frames.extend(silence(5));
    // Fewer voiced frames than min_voiced_frames: a blip, not an utterance.
    let blip_len = cfg.min_voiced_frames as usize - 5;
    frames.extend(speech_like_frames(blip_len));
    frames.extend(silence(60));

    for frame in frames {
        assert_ne!(
            det.push_frame(&frame).unwrap(),
            DetectorEvent::SpeechStart,
            "a blip shorter than min_voiced_frames must never start an utterance"
        );
    }
}

// ── BargeInDetector ──────────────────────────────────────────────────────

#[test]
fn bargein_confirmation_timing_is_within_the_confirm_window() {
    let cfg = BargeInConfig::default();
    let mut det = BargeInDetector::new(cfg);
    // Headphones: no acoustic loop, so ERLE/route/warmup gates are all
    // bypassed and only the confirmation window + spectral check matter --
    // isolating pure attack latency.
    det.set_output_route(OutputRoute::Headphones);

    let lead_in = 3;
    let mut frames = Vec::new();
    frames.extend(silence(lead_in));
    frames.extend(speech_like_frames(30));

    let events: Vec<DetectorEvent> = frames.iter().map(|f| det.push_frame(f).unwrap()).collect();

    // The window needs `confirm_window` frames to fill, so the earliest
    // SpeechStart can appear is at onset + confirm_window - 1.
    let onset = lead_in;
    let start_idx = onset + cfg.confirm_window - 1;

    for e in &events[..start_idx] {
        assert_ne!(
            *e,
            DetectorEvent::SpeechStart,
            "must not confirm before the window fills"
        );
    }
    assert_eq!(events[start_idx], DetectorEvent::SpeechStart);

    let latency_ms = cfg.confirm_window as f32 * 10.0;
    assert!(
        latency_ms <= 40.0,
        "barge-in attack latency must stay within the ~40ms product budget, got {latency_ms}ms"
    );
}

#[test]
fn bargein_does_not_fire_on_synthetic_cough() {
    let mut det = BargeInDetector::new(BargeInConfig::default());
    det.set_output_route(OutputRoute::Speakers);
    det.set_aec_stats(good_stats(20.0)); // ERLE gate open: isolate the spectral check.

    let mut frames = Vec::new();
    frames.extend(silence(5));
    frames.extend(cough_frames(8)); // ~80ms
    frames.extend(silence(60));

    for frame in frames {
        assert_ne!(
            det.push_frame(&frame).unwrap(),
            DetectorEvent::SpeechStart,
            "a stationary broadband impulse must not confirm as speech"
        );
    }
}

#[test]
fn bargein_does_not_fire_when_erle_below_floor() {
    let cfg = BargeInConfig::default();
    let mut det = BargeInDetector::new(cfg);
    det.set_output_route(OutputRoute::Speakers);
    det.set_aec_stats(good_stats(cfg.erle_floor_db - 5.0)); // clearly below floor

    let mut frames = Vec::new();
    frames.extend(silence(3));
    frames.extend(speech_like_frames(30)); // otherwise-clear speech

    for frame in frames {
        assert_ne!(
            det.push_frame(&frame).unwrap(),
            DetectorEvent::SpeechStart,
            "a canceller that isn't keeping up must suppress acoustic detection entirely"
        );
    }
}

#[test]
fn bargein_fires_on_headphones_despite_absent_erle() {
    let cfg = BargeInConfig::default();
    let mut det = BargeInDetector::new(cfg);
    det.set_output_route(OutputRoute::Headphones);
    // Deliberately never call set_aec_stats(): erle_db stays None ("absent").

    let lead_in = 3;
    let mut frames = Vec::new();
    frames.extend(silence(lead_in));
    frames.extend(speech_like_frames(30));

    let events: Vec<DetectorEvent> = frames.iter().map(|f| det.push_frame(f).unwrap()).collect();

    let start_idx = lead_in + cfg.confirm_window - 1;
    assert_eq!(
        events[start_idx],
        DetectorEvent::SpeechStart,
        "headphones must bypass the ERLE gate via the relaxed path"
    );
}

#[test]
fn bargein_does_not_fire_during_convergence_warmup_then_fires_after() {
    let cfg = BargeInConfig::default();
    let mut det = BargeInDetector::new(cfg);
    det.set_output_route(OutputRoute::Speakers);
    det.set_aec_stats(good_stats(20.0)); // ERLE gate would otherwise be open immediately.
    det.notify_playback_started();

    let frames = speech_like_frames(40);
    let events: Vec<DetectorEvent> = frames.iter().map(|f| det.push_frame(f).unwrap()).collect();

    // warmup_remaining decrements to 0 on the `convergence_warmup_frames`th
    // push, so that is the earliest frame the guard can lift on.
    let warmup_lifts_at = cfg.convergence_warmup_frames as usize - 1;
    for e in &events[..warmup_lifts_at] {
        assert_ne!(
            *e,
            DetectorEvent::SpeechStart,
            "must not confirm acoustically during the post-playback convergence warm-up"
        );
    }
    assert_eq!(
        events[warmup_lifts_at],
        DetectorEvent::SpeechStart,
        "must confirm as soon as the warm-up elapses, given the window was already satisfied"
    );
}

#[test]
fn bargein_pure_silence_produces_no_detection_ever() {
    let mut det = BargeInDetector::new(BargeInConfig::default());
    det.set_output_route(OutputRoute::Headphones); // most permissive gating; still must not fire
    for frame in silence(200) {
        assert_eq!(det.push_frame(&frame).unwrap(), DetectorEvent::Idle);
    }
}
