//! Adversarial fixtures for the barge-in detector.
//!
//! The existing cough fixture in `detection.rs` is a constant sum of four tones
//! with continuous phase and no envelope. Its zero-crossing rate is therefore
//! identical in every frame, so its ZCR variance is zero *by construction* and
//! the spectral-variation gate rejects it without being tested. That test can
//! never fail, which means it never told us anything.
//!
//! Real transients do not look like that. A cough is a broadband burst with a
//! very fast attack (a few ms) and an exponential decay over 50–150 ms. A
//! keyboard click is shorter and sharper. A door slam is louder and longer.
//! All three have strongly time-varying spectra during the decay, which is
//! exactly the region where a ZCR-variance gate is weakest.
//!
//! These are still synthetic — no recordings are available in this environment —
//! but they are synthetic in the ways that make the gate work for its living.

use buzztalk_core::{
    AecStats, DetectorEvent, OutputRoute, SpeechDetector, FRAME_SAMPLES, SAMPLE_RATE_HZ,
};
use buzztalk_vad::{BargeInConfig, BargeInDetector};

/// Deterministic noise, so a failure is reproducible.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.0 >> 33) as f32 / (1u64 << 31) as f32) - 1.0
    }
}

/// Broadband transient: near-instant attack, exponential decay, spectrally
/// evolving as the high frequencies die first — the shape a real cough,
/// clap, or slam actually has.
fn transient_frames(n_frames: usize, peak: f32, decay_tau_ms: f32, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = Lcg(seed);
    let mut frames = Vec::with_capacity(n_frames);
    let mut idx = 0usize;
    // One-pole low-pass whose cutoff falls over time, so the burst gets duller
    // as it decays. That is the time-varying spectrum the gate must survive.
    let mut lp = 0.0f32;
    for _ in 0..n_frames {
        let mut frame = vec![0.0f32; FRAME_SAMPLES];
        for s in frame.iter_mut() {
            let t_ms = idx as f32 * 1000.0 / SAMPLE_RATE_HZ as f32;
            // 3 ms attack, then exponential decay.
            let env = if t_ms < 3.0 {
                t_ms / 3.0
            } else {
                (-(t_ms - 3.0) / decay_tau_ms).exp()
            };
            let white = rng.next_f32();
            // Cutoff coefficient sweeps from wide open to dull as it decays.
            let alpha = 0.9 * env.max(0.05) + 0.05;
            lp += alpha * (white - lp);
            *s = lp * env * peak;
            idx += 1;
        }
        frames.push(frame);
    }
    frames
}

/// Sustained voiced speech: harmonic stack with vibrato and a moving formant,
/// so its ZCR genuinely varies frame to frame the way real speech does.
fn speech_frames(n_frames: usize) -> Vec<Vec<f32>> {
    let mut frames = Vec::with_capacity(n_frames);
    let mut idx = 0usize;
    for _ in 0..n_frames {
        let mut frame = vec![0.0f32; FRAME_SAMPLES];
        for s in frame.iter_mut() {
            let t = idx as f32 / SAMPLE_RATE_HZ as f32;
            // Pitch drifts (vibrato), formant sweeps — both move the ZCR.
            let f0 = 130.0 + 12.0 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
            let formant = 900.0 + 500.0 * (2.0 * std::f32::consts::PI * 1.7 * t).sin();
            let v = 0.35 * (2.0 * std::f32::consts::PI * f0 * t).sin()
                + 0.22 * (2.0 * std::f32::consts::PI * 2.0 * f0 * t).sin()
                + 0.18 * (2.0 * std::f32::consts::PI * formant * t).sin();
            *s = v;
            idx += 1;
        }
        frames.push(frame);
    }
    frames
}

fn armed_detector() -> BargeInDetector {
    let mut d = BargeInDetector::new(BargeInConfig::default());
    // Converged canceller, loudspeaker route: the strict path, gates active.
    d.set_aec_stats(AecStats {
        erle_db: Some(25.0),
        estimated_delay_ms: Some(40),
        double_talk: false,
    });
    d.set_output_route(OutputRoute::Speakers);
    d.notify_playback_started();
    // Push past the convergence warm-up so it is not what suppresses detection.
    for _ in 0..40 {
        let _ = d.push_frame(&vec![0.0f32; FRAME_SAMPLES]);
    }
    d
}

fn fires(detector: &mut BargeInDetector, frames: &[Vec<f32>]) -> bool {
    frames.iter().any(|f| {
        matches!(
            detector.push_frame(f).expect("frame is well formed"),
            DetectorEvent::SpeechStart
        )
    })
}

#[test]
fn control_real_speech_still_fires() {
    // If this fails, the fixtures below prove nothing: a detector that rejects
    // everything trivially "rejects coughs" too.
    let mut d = armed_detector();
    assert!(
        fires(&mut d, &speech_frames(40)),
        "barge-in must fire on sustained voiced speech, or the rejection tests are meaningless"
    );
}

#[test]
fn a_broadband_cough_does_not_fire_barge_in() {
    // ~90 ms cough: loud, fast attack, decaying, spectrally evolving.
    let mut d = armed_detector();
    let cough = transient_frames(9, 0.9, 25.0, 0xC0FFEE);
    assert!(
        !fires(&mut d, &cough),
        "a broadband cough triggered barge-in — the agent would be cut off mid-sentence"
    );
}

#[test]
fn a_keyboard_click_does_not_fire_barge_in() {
    // ~30 ms, sharper and quieter than a cough.
    let mut d = armed_detector();
    let click = transient_frames(3, 0.6, 8.0, 0xBEEF);
    assert!(
        !fires(&mut d, &click),
        "a keyboard click triggered barge-in"
    );
}

#[test]
fn a_door_slam_does_not_fire_barge_in() {
    // Louder and longer than a cough — the hardest of the three to reject,
    // because sheer energy is what a naive detector keys on.
    let mut d = armed_detector();
    let slam = transient_frames(15, 1.0, 60.0, 0xD005);
    assert!(!fires(&mut d, &slam), "a door slam triggered barge-in");
}

#[test]
fn repeated_transients_do_not_accumulate_into_a_false_fire() {
    // Someone typing: a burst every ~200 ms. State must not creep across them.
    let mut d = armed_detector();
    let mut fired = false;
    for seed in 0..8u64 {
        let click = transient_frames(3, 0.6, 8.0, 0x1000 + seed);
        if fires(&mut d, &click) {
            fired = true;
        }
        for _ in 0..17 {
            let _ = d.push_frame(&vec![0.0f32; FRAME_SAMPLES]);
        }
    }
    assert!(
        !fired,
        "a burst of typing accumulated into a false barge-in"
    );
}
