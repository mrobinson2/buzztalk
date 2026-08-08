//! Phase 0 harness: measure echo cancellation offline, with no audio hardware.
//!
//! This exists to answer one question before any product code is written: do
//! the candidate echo-cancellation backends actually cancel echo, and by how
//! much? A backend that builds but does not converge is worse than no backend,
//! because it would silently license us to leave the microphone open while the
//! agent talks.
//!
//! Method: synthesise a far-end signal, derive a fake near-end from it by
//! delaying, attenuating and filtering it (a crude room), add a little noise,
//! then feed far-end to `process_render` and near-end to `process_capture`.
//! ERLE is the ratio of input echo energy to residual output energy, in dB,
//! measured only over the converged tail so start-up transients do not flatter
//! the result.
//!
//! Run: `cargo run -p buzztalk-labs --bin erle-bench`

use buzztalk_core::{AecStats, EchoCanceller, Result, FRAME_SAMPLES, SAMPLE_RATE_HZ};

// ── Signal generation ─────────────────────────────────────────────────────────

/// Deterministic pseudo-random source. Reproducible runs matter more here than
/// statistical quality — two runs of the bench must be comparable.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        ((self.0 >> 33) as f32 / (1u64 << 31) as f32) - 1.0
    }
}

/// Speech-like far-end: band-limited noise, amplitude-modulated at syllable
/// rate, with pauses. Closer to real speech than a sine, and unlike a chirp it
/// exercises the canceller's double-talk and pause handling.
fn synth_far_end(seconds: f32) -> Vec<f32> {
    let n = (seconds * SAMPLE_RATE_HZ as f32) as usize;
    let mut rng = Lcg::new(0x5EED_1234);
    let mut out = Vec::with_capacity(n);
    // One-pole low-pass state, to band-limit white noise toward voice range.
    let mut lp = 0.0f32;
    let mut hp_prev_in = 0.0f32;
    let mut hp_prev_out = 0.0f32;
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE_HZ as f32;
        let white = rng.next_f32();
        // ~3.5 kHz low-pass.
        lp += 0.35 * (white - lp);
        // ~120 Hz high-pass, removing the DC rumble noise alone would carry.
        let hp = 0.98 * (hp_prev_out + lp - hp_prev_in);
        hp_prev_in = lp;
        hp_prev_out = hp;
        // Syllable-rate envelope (~4 Hz) with silent stretches every ~2.5 s.
        let syllable = (1.0 + (2.0 * std::f32::consts::PI * 4.0 * t).sin()) * 0.5;
        let phrase = if (t % 2.5) > 2.0 { 0.0 } else { 1.0 };
        out.push(hp * syllable * phrase * 0.5);
    }
    out
}

/// Simulate a loudspeaker-to-microphone path: pure delay, attenuation, a couple
/// of reflections, and a mild non-linearity. The non-linearity matters — it is
/// exactly why naive subtraction of the known playback signal fails on real
/// hardware, and a backend that only handles the linear case will look great
/// here and disappoint in a room.
fn simulate_echo(far: &[f32], delay_samples: usize, gain_db: f32) -> Vec<f32> {
    let gain = 10f32.powf(gain_db / 20.0);
    let mut rng = Lcg::new(0xC0FFEE);
    let mut near = vec![0.0f32; far.len()];
    let reflections = [(delay_samples, 1.0f32), (delay_samples + 617, 0.35), (delay_samples + 1451, 0.15)];
    for (d, g) in reflections {
        for i in d..far.len() {
            near[i] += far[i - d] * g * gain;
        }
    }
    for s in near.iter_mut() {
        // Gentle soft-clip: a real speaker is not linear at conversational volume.
        *s = s.tanh();
        // Ambient noise floor, ~ -60 dBFS.
        *s += rng.next_f32() * 0.001;
    }
    near
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|s| (*s as f64) * (*s as f64)).sum()
}

fn db_ratio(input: f64, output: f64) -> f32 {
    if output <= f64::EPSILON {
        return f32::INFINITY;
    }
    (10.0 * (input / output).log10()) as f32
}

// ── Measurement ───────────────────────────────────────────────────────────────

/// What one backend scored.
pub struct BenchResult {
    pub name: &'static str,
    /// Echo reduction measured over the converged tail, in dB. Higher is better.
    pub erle_db: f32,
    /// What the backend itself claimed, if it reports ERLE.
    pub self_reported: AecStats,
}

/// Run the synthetic bake-off against one canceller.
///
/// `converge_fraction` is the leading portion of the signal excluded from
/// measurement, so we score steady-state behaviour rather than convergence
/// speed. Convergence speed is measured separately because it is what gates
/// barge-in in the first ~150 ms of an utterance.
pub fn bench(
    mut aec: Box<dyn EchoCanceller>,
    seconds: f32,
    delay_ms: u32,
    echo_gain_db: f32,
    converge_fraction: f32,
) -> Result<BenchResult> {
    let name = aec.name();
    let far = synth_far_end(seconds);
    let delay_samples = (delay_ms as usize * SAMPLE_RATE_HZ as usize) / 1000;
    let near = simulate_echo(&far, delay_samples, echo_gain_db);

    aec.set_stream_delay_ms(delay_ms);

    let frames = far.len() / FRAME_SAMPLES;
    let skip = (frames as f32 * converge_fraction) as usize;

    let mut input_energy = 0.0f64;
    let mut output_energy = 0.0f64;
    let mut scratch = vec![0.0f32; FRAME_SAMPLES];

    for f in 0..frames {
        let lo = f * FRAME_SAMPLES;
        let hi = lo + FRAME_SAMPLES;
        aec.process_render(&far[lo..hi])?;
        scratch.copy_from_slice(&near[lo..hi]);
        aec.process_capture(&mut scratch)?;
        if f >= skip {
            input_energy += energy(&near[lo..hi]);
            output_energy += energy(&scratch);
        }
    }

    Ok(BenchResult {
        name,
        erle_db: db_ratio(input_energy, output_energy),
        self_reported: aec.stats(),
    })
}

// ── Control condition ─────────────────────────────────────────────────────────

/// Passthrough canceller, local to the harness so the bench can run before any
/// real backend exists. It must score ~0 dB; if it does not, the harness itself
/// is wrong and every other number it prints is meaningless.
struct Passthrough;

impl EchoCanceller for Passthrough {
    fn process_render(&mut self, _far_end: &[f32]) -> Result<()> {
        Ok(())
    }
    fn process_capture(&mut self, _near_end: &mut [f32]) -> Result<()> {
        Ok(())
    }
    fn set_stream_delay_ms(&mut self, _delay_ms: u32) {}
    fn stats(&self) -> AecStats {
        AecStats::default()
    }
    fn name(&self) -> &'static str {
        "passthrough(control)"
    }
}

fn main() -> Result<()> {
    println!("BuzzTalk Phase 0 — echo cancellation bake-off");
    println!("{:-<64}", "");
    println!(
        "signal: 12 s speech-like far-end, echo at 40 ms / -12 dB,\n\
         3 reflections, soft-clip non-linearity, -60 dBFS noise floor\n\
         scored over the final 50% of the run (post-convergence)\n"
    );

    // `mut` is only used when real backends are compiled in; the control alone
    // needs no mutation.
    #[allow(unused_mut)]
    let mut results: Vec<BenchResult> = vec![bench(Box::new(Passthrough), 12.0, 40, -12.0, 0.5)?];

    // Real backends are appended here once `buzztalk-aec` lands. Keeping the
    // control in the same table is deliberate: a backend is only interesting
    // relative to doing nothing.
    #[cfg(feature = "aec-backends")]
    {
        for name in buzztalk_aec::available_backends() {
            results.push(bench(
                buzztalk_aec::new_backend(name)?,
                12.0,
                40,
                -12.0,
                0.5,
            )?);
        }
    }

    println!("{:<28} {:>12}  {:>14}", "backend", "ERLE (dB)", "self-reported");
    println!("{:-<64}", "");
    for r in &results {
        let claimed = r
            .self_reported
            .erle_db
            .map(|v| format!("{v:.1} dB"))
            .unwrap_or_else(|| "-".to_string());
        println!("{:<28} {:>12.1}  {:>14}", r.name, r.erle_db, claimed);
    }
    println!("{:-<64}", "");
    println!("gate: a usable backend scores > 20 dB here and > 12 dB in a real room.");
    println!("the control must score ~0 dB, or this harness is lying to you.");

    Ok(())
}
