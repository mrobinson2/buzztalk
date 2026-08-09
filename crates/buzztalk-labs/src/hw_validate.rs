//! Acoustic validation. **Run this on a Mac with a real microphone and real speakers.**
//!
//! Every echo-cancellation number in this repository so far is synthetic, measured
//! against a simulated echo path on a machine whose audio device is virtual. They
//! show the algorithm works. They do not show the product works in a room, and the
//! difference is the whole feature: barge-in depends on the canceller subtracting a
//! real loudspeaker leaking into a real microphone, with real non-linearity, real
//! room reflections and real device clock drift.
//!
//! This binary closes that gap. It runs four checks and prints a verdict for each.
//! It needs a quiet-ish room and about two minutes.
//!
//! ```text
//! cargo run --release -p buzztalk-labs --bin hw-validate --features aec-backends
//! ```

use std::time::{Duration, Instant};

use buzztalk_audio::{detect_output_route, DuplexConfig, DuplexEngine};
use buzztalk_core::{OutputRoute, FRAME_SAMPLES, SAMPLE_RATE_HZ};

const SPEECH_BAND_LO: f32 = 300.0;
const SPEECH_BAND_HI: f32 = 3400.0;

fn banner(step: &str, what: &str) {
    println!("\n{:=<72}", "");
    println!("{step}  {what}");
    println!("{:=<72}", "");
}

fn prompt(msg: &str) {
    println!("\n>>> {msg}");
    println!(">>> press Enter when ready");
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}

/// Speech-band noise, so the canceller sees excitation like the voice it will
/// actually have to cancel. A pure tone is the classic mistake here: adaptive
/// filters decline to adapt on narrowband input and the result looks broken.
fn probe_chunk(state: &mut (f32, u64), samples: usize) -> Vec<f32> {
    let (ref mut lp, ref mut rng) = *state;
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        *rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let white = ((*rng >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
        let cutoff = SPEECH_BAND_HI / (SAMPLE_RATE_HZ as f32 / 2.0);
        *lp += cutoff * (white - *lp);
        out.push(*lp * 0.35);
    }
    out
}

fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let mean_sq: f64 = samples
        .iter()
        .map(|s| (*s as f64) * (*s as f64))
        .sum::<f64>()
        / samples.len() as f64;
    if mean_sq <= 1e-20 {
        return -120.0;
    }
    10.0 * mean_sq.log10() as f32
}

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("BuzzTalk acoustic validation");
    println!("This must run on a Mac with a REAL microphone and REAL speakers.");
    println!("Sound will play. Keep the room reasonably quiet.");
    println!("Speech band assumed {SPEECH_BAND_LO:.0}-{SPEECH_BAND_HI:.0} Hz.");

    let mut checks: Vec<Check> = Vec::new();

    // ── 1. Is there real hardware at all? ────────────────────────────────────
    banner("STEP 1", "device inventory");
    let devices = buzztalk_audio::enumerate_devices()?;
    println!("inputs : {:?}", devices.input);
    println!("outputs: {:?}", devices.output);
    let virtualish = [
        "jump desktop",
        "blackhole",
        "loopback",
        "soundflower",
        "aggregate",
    ];
    let looks_virtual = devices
        .input
        .iter()
        .all(|d| virtualish.iter().any(|v| d.to_lowercase().contains(v)));
    checks.push(Check {
        name: "real input device present",
        passed: !looks_virtual && !devices.input.is_empty(),
        detail: if looks_virtual {
            "every input looks like a virtual driver — results below will be meaningless".into()
        } else {
            format!("{} input(s)", devices.input.len())
        },
    });

    // ── 2. Route detection ───────────────────────────────────────────────────
    banner("STEP 2", "output route detection");
    println!("Current route: {}", detect_output_route());
    prompt("Unplug headphones so audio plays from SPEAKERS, then continue");
    let speakers = detect_output_route();
    println!("  detected: {speakers}");
    prompt("Now PLUG IN headphones, then continue");
    let headphones = detect_output_route();
    println!("  detected: {headphones}");
    let route_ok = headphones == OutputRoute::Headphones && speakers != OutputRoute::Headphones;
    checks.push(Check {
        name: "route detection distinguishes headphones from speakers",
        passed: route_ok,
        detail: format!("speakers => {speakers}, headphones => {headphones}"),
    });
    if !route_ok {
        println!("  NOTE: the headphone fast path is the demo's safety net. If this is wrong,");
        println!("  barge-in will be gated on ERLE even when there is no echo path at all.");
    }

    // ── 3. The real measurement: ERLE over a live acoustic path ──────────────
    banner(
        "STEP 3",
        "echo cancellation through air (the one that matters)",
    );
    prompt("Unplug headphones. Set volume to a normal conversational level. Then stay QUIET");

    let mut engine = DuplexEngine::start(DuplexConfig::default())?;
    let mut aec = buzztalk_aec::new_best_available();
    if let Some(ms) = engine.output_latency_ms() {
        aec.set_stream_delay_ms(ms);
    }
    println!(
        "backend: {}  output latency: {:?} ms",
        aec.name(),
        engine.output_latency_ms()
    );

    let mut noise = (0.0f32, 0x5EEDu64);
    let mut pushed: u64 = 0;
    let lead = (FRAME_SAMPLES * 4) as u64;
    let mut pre: Vec<f32> = Vec::new();
    let mut post: Vec<f32> = Vec::new();
    let mut pending_ref: Vec<Vec<f32>> = Vec::new();
    let mut pending_cap: Vec<Vec<f32>> = Vec::new();

    let start = Instant::now();
    let run_for = Duration::from_secs(12);
    // Skip the first third: the canceller must converge before it is scored.
    let score_after = Duration::from_secs(4);

    while start.elapsed() < run_for {
        let wall = (start.elapsed().as_secs_f64() * SAMPLE_RATE_HZ as f64) as u64;
        if pushed < wall + lead {
            let want = (wall + lead - pushed) as usize;
            engine.push_playback(&probe_chunk(&mut noise, want));
            pushed += want as u64;
        }
        while let Some(f) = engine.try_recv_render_ref() {
            pending_ref.push(f);
        }
        while let Some(f) = engine.try_recv_capture() {
            pending_cap.push(f);
        }
        while !pending_ref.is_empty() && !pending_cap.is_empty() {
            let far = pending_ref.remove(0);
            let mut near = pending_cap.remove(0);
            aec.process_render(&far)?;
            let before = near.clone();
            aec.process_capture(&mut near)?;
            if start.elapsed() > score_after {
                pre.extend_from_slice(&before);
                post.extend_from_slice(&near);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let pre_db = rms_dbfs(&pre);
    let post_db = rms_dbfs(&post);
    let erle = pre_db - post_db;
    let stats = aec.stats();
    println!("\n  captured echo   : {pre_db:.1} dBFS");
    println!("  after cancelling: {post_db:.1} dBFS");
    println!(
        "  MEASURED ERLE   : {erle:.1} dB   (backend self-reports {:?})",
        stats.erle_db
    );

    // If the mic heard essentially nothing, there was no echo path to cancel and
    // the ERLE figure is meaningless rather than good.
    let heard_echo = pre_db > -60.0;
    checks.push(Check {
        name: "microphone actually hears the speaker",
        passed: heard_echo,
        detail: format!("{pre_db:.1} dBFS captured while playing"),
    });
    checks.push(Check {
        name: "real-world ERLE >= 12 dB (barge-in gate threshold)",
        passed: heard_echo && erle >= 12.0,
        detail: format!("{erle:.1} dB"),
    });

    // ── 4. Does the canceller's self-report track reality? ───────────────────
    // This matters more than it looks: BargeInDetector gates on the reported
    // value, so a backend that under-reports disables barge-in while cancelling
    // perfectly well. That is exactly how the webrtc backend failed.
    banner("STEP 4", "self-reported ERLE vs measured");
    let reported_ok = match stats.erle_db {
        Some(r) => {
            println!("  reported {r:.1} dB vs measured {erle:.1} dB");
            (r - erle).abs() < 15.0
        }
        None => {
            println!("  backend reports no ERLE at all — the barge-in gate cannot use it");
            false
        }
    };
    checks.push(Check {
        name: "self-reported ERLE tracks measured (gate depends on it)",
        passed: reported_ok,
        detail: match stats.erle_db {
            Some(r) => format!("reported {r:.1}, measured {erle:.1}"),
            None => "not reported".into(),
        },
    });

    // ── Verdict ──────────────────────────────────────────────────────────────
    banner("VERDICT", "");
    let mut all = true;
    for c in &checks {
        println!(
            "  [{}] {:<52} {}",
            if c.passed { "PASS" } else { "FAIL" },
            c.name,
            c.detail
        );
        all &= c.passed;
    }
    println!();
    if all {
        println!("ALL CHECKS PASSED — the acoustic claims can be made honestly.");
        println!("Update README and docs/PHASE-0.md to replace 'synthetic' with these numbers,");
        println!("and drop the -alpha suffix once barge-in is confirmed by ear as well.");
    } else {
        println!("SOME CHECKS FAILED — do not drop the -alpha suffix, and do not");
        println!("weaken the 'use headphones' guidance in the release notes.");
    }

    println!("\nNext, by ear: run");
    println!("  buzztalk-demo --seconds 60");
    println!("with speakers on, let the agent talk, and interrupt it out loud.");
    println!("If it stops, barge-in works in a room. That is the last claim we cannot automate.");

    Ok(())
}
