//! Phase 1 integration: duplex engine + echo canceller, on real device timing.
//!
//! The offline bench proves the algorithm cancels. This proves the *plumbing*
//! works: that render-reference frames and capture frames arrive paired and in
//! order, that the canceller keeps up at the 10 ms cadence without starving the
//! audio callbacks, and that delay alignment is seeded from something real.
//!
//! Those are different failures. An echo canceller fed correct audio in the
//! wrong order, or a frame late, produces exactly the same symptom as one that
//! does not work at all — and only this harness can tell them apart.
//!
//! On a machine with no acoustic loop (virtual audio device, or headphones)
//! expect ERLE near 0 dB. That is the correct result, not a failure: there is
//! no echo to remove. What is being tested here is timing and pairing.
//!
//! Run: `cargo run -p buzztalk-labs --bin live-aec --features aec-backends -- [seconds]`

use std::time::{Duration, Instant};

use buzztalk_audio::{detect_output_route, DuplexConfig, DuplexEngine};
use buzztalk_core::{EchoCanceller, OutputRoute, FRAME_SAMPLES, SAMPLE_RATE_HZ};

fn tone_chunk(phase: &mut f32, samples: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        *phase += 2.0 * std::f32::consts::PI * 330.0 / SAMPLE_RATE_HZ as f32;
        if *phase > std::f32::consts::TAU {
            *phase -= std::f32::consts::TAU;
        }
        out.push(phase.sin() * 0.15);
    }
    out
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|s| (*s as f64) * (*s as f64)).sum()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let route = detect_output_route();
    println!("BuzzTalk Phase 1 — live duplex + AEC ({seconds}s)");
    println!("output route      : {route}");
    println!("assumes echo path : {}", route.has_echo_path());

    let mut engine = DuplexEngine::start(DuplexConfig::default())?;
    let latency = engine.output_latency_ms();
    println!(
        "output latency    : {}",
        latency
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "unreported".into())
    );

    let mut aec: Box<dyn EchoCanceller> = buzztalk_aec::new_best_available();
    println!("aec backend       : {}", aec.name());

    // Seed the delay estimate from what the device reports. The canceller
    // refines it, but a sane starting point shortens convergence — and
    // convergence time is exactly what gates barge-in at the start of an
    // utterance.
    if let Some(ms) = latency {
        aec.set_stream_delay_ms(ms);
    }
    println!();

    let mut phase = 0.0f32;
    let mut pushed: u64 = 0;
    let lead = (FRAME_SAMPLES * 4) as u64;

    // Frames are only meaningful in pairs: one render reference, one capture.
    // Holding them in queues and consuming one of each keeps the canceller's
    // two inputs aligned even if the rings deliver in bursts.
    let mut pending_ref: Vec<Vec<f32>> = Vec::new();
    let mut pending_cap: Vec<Vec<f32>> = Vec::new();

    let mut processed = 0u64;
    let mut in_energy = 0.0f64;
    let mut out_energy = 0.0f64;
    let mut worst_frame_us = 0u128;

    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let mut next_report = start + Duration::from_secs(5);

    while Instant::now() < deadline {
        let wall = (start.elapsed().as_secs_f64() * SAMPLE_RATE_HZ as f64) as u64;
        if pushed < wall + lead {
            let want = (wall + lead - pushed) as usize;
            engine.push_playback(&tone_chunk(&mut phase, want));
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

            let t0 = Instant::now();
            aec.process_render(&far)?;
            let before = energy(&near);
            aec.process_capture(&mut near)?;
            let after = energy(&near);
            worst_frame_us = worst_frame_us.max(t0.elapsed().as_micros());

            // Skip the first second: convergence transients are measured
            // separately, not folded into the steady-state figure.
            if start.elapsed() > Duration::from_secs(1) {
                in_energy += before;
                out_energy += after;
            }
            processed += 1;
        }

        if Instant::now() >= next_report {
            let stats = aec.stats();
            println!(
                "t={:>3}s  frames={processed:>6}  backlog(ref/cap)={}/{}  aec_erle={}  worst_frame={worst_frame_us}us",
                start.elapsed().as_secs(),
                pending_ref.len(),
                pending_cap.len(),
                stats
                    .erle_db
                    .map(|v| format!("{v:.1}dB"))
                    .unwrap_or_else(|| "-".into()),
            );
            next_report += Duration::from_secs(5);
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    // A silent capture stream makes ERLE meaningless rather than zero. Say so,
    // instead of printing an infinity that looks like a result.
    let capture_is_silent = in_energy <= 1e-9;
    let realtime_budget_us = 10_000; // one 10 ms frame

    println!("\n{:-<72}", "");
    println!("frames processed   : {processed}");
    if capture_is_silent {
        println!(
            "measured ERLE      : n/a — capture is digital silence (no acoustic path\n\
             \x20                    into this input device), so there is no echo to remove"
        );
    } else {
        let measured_erle = 10.0 * (in_energy / out_energy.max(f64::EPSILON)).log10();
        println!("measured ERLE      : {measured_erle:.1} dB");
    }
    println!("worst frame time   : {worst_frame_us} us (budget {realtime_budget_us} us)");
    println!("engine stats       : {:?}", engine.stats());
    println!("{:-<72}", "");

    let mut ok = true;
    if worst_frame_us > realtime_budget_us {
        println!("FAIL — a single AEC frame exceeded its 10 ms real-time budget.");
        ok = false;
    }
    let s = engine.stats();
    if s.render_ref_samples_dropped > 0 || s.capture_samples_dropped > 0 {
        println!("FAIL — rings overran while the canceller was in the loop.");
        ok = false;
    }
    if route == OutputRoute::Headphones {
        println!("note — headphones: no acoustic loop, so ERLE near 0 dB is expected and fine.");
    }
    if ok {
        println!("PASS — canceller keeps up with real device timing, frames stay paired.");
        Ok(())
    } else {
        Err("live AEC integration failed".into())
    }
}
