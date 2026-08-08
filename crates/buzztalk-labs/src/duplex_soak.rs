//! Phase 0 gate 4: run the duplex engine continuously and prove it is stable.
//!
//! What this catches that a unit test cannot: clock drift between the input and
//! output devices, ring overruns under real callback timing, and — the one that
//! actually breaks echo cancellation — gaps in the render reference. A canceller
//! fed a reference with holes in it diverges, and the symptom shows up much
//! later as "barge-in randomly stops working".
//!
//! The reference stream must stay locked to the capture stream at 1:1 over
//! minutes, not just over a buffer. That invariant is the whole point.
//!
//! Run: `cargo run -p buzztalk-labs --bin duplex-soak -- [seconds]`

use std::time::{Duration, Instant};

use buzztalk_audio::{detect_output_route, DuplexConfig, DuplexEngine};
use buzztalk_core::FRAME_SAMPLES;

/// Playback tone, so the render path carries real non-silent samples rather
/// than only exercising the silence-fill branch.
fn tone_chunk(phase: &mut f32, samples: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        *phase += 2.0 * std::f32::consts::PI * 220.0 / buzztalk_core::SAMPLE_RATE_HZ as f32;
        if *phase > std::f32::consts::TAU {
            *phase -= std::f32::consts::TAU;
        }
        out.push(phase.sin() * 0.05);
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    println!("BuzzTalk Phase 0 — duplex soak ({seconds}s)");
    println!("output route: {}", detect_output_route());

    let mut engine = DuplexEngine::start(DuplexConfig::default())?;
    println!(
        "engine started. output latency: {}",
        engine
            .output_latency_ms()
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "unreported".into())
    );

    let mut phase = 0.0f32;
    let mut capture_frames = 0u64;
    let mut render_frames = 0u64;
    let mut playback_dropped = 0u64;

    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let mut next_report = start + Duration::from_secs(10);

    // Audio pushed so far, in samples. Paced against the wall clock: pushing
    // faster than real time just exercises the drop counter and tells us
    // nothing about whether playback would actually starve.
    let mut pushed_samples: u64 = 0;
    let lead_samples = (FRAME_SAMPLES * 4) as u64; // keep ~40 ms queued ahead

    while Instant::now() < deadline {
        let wall_samples =
            (start.elapsed().as_secs_f64() * buzztalk_core::SAMPLE_RATE_HZ as f64) as u64;
        if pushed_samples < wall_samples + lead_samples {
            let want = (wall_samples + lead_samples - pushed_samples) as usize;
            playback_dropped += engine.push_playback(&tone_chunk(&mut phase, want)) as u64;
            pushed_samples += want as u64;
        }

        while engine.try_recv_capture().is_some() {
            capture_frames += 1;
        }
        while engine.try_recv_render_ref().is_some() {
            render_frames += 1;
        }

        if Instant::now() >= next_report {
            let s = engine.stats();
            let skew = capture_frames as i64 - render_frames as i64;
            println!(
                "t={:>3}s  capture={capture_frames:>7}  render_ref={render_frames:>7}  skew={skew:>+6}  \
                 drops(cap/ref/play)={}/{}/{}  underrun={}",
                start.elapsed().as_secs(),
                s.capture_samples_dropped,
                s.render_ref_samples_dropped,
                s.playback_samples_dropped,
                s.playback_underrun_samples,
            );
            next_report += Duration::from_secs(10);
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let s = engine.stats();
    let skew = capture_frames as i64 - render_frames as i64;
    let elapsed = start.elapsed().as_secs_f64();

    println!("\n{:-<72}", "");
    println!("ran {elapsed:.1}s");
    println!("capture frames      : {capture_frames}");
    println!("render-ref frames   : {render_frames}");
    println!(
        "skew (cap - ref)    : {skew:+} frames ({:.1} ms)",
        skew as f64 * 10.0
    );
    println!(
        "capture dropped     : {} samples",
        s.capture_samples_dropped
    );
    println!(
        "render-ref dropped  : {} samples",
        s.render_ref_samples_dropped
    );
    println!(
        "playback dropped    : {} samples (harness backpressure: {playback_dropped})",
        s.playback_samples_dropped
    );
    println!(
        "playback underrun   : {} samples",
        s.playback_underrun_samples
    );
    println!("{:-<72}", "");

    // Gate 4. Drift is the failure that matters: a reference stream that slips
    // relative to capture silently destroys echo cancellation.
    let mut failures = Vec::new();
    if s.capture_samples_dropped > 0 {
        failures.push("capture ring overran");
    }
    if s.render_ref_samples_dropped > 0 {
        failures.push("render-reference ring overran — AEC would diverge");
    }
    if skew.unsigned_abs() > 50 {
        failures.push("capture and render reference drifted apart by >500 ms");
    }

    if failures.is_empty() {
        println!("GATE 4 PASS — duplex stable, reference locked to capture.");
        Ok(())
    } else {
        for f in &failures {
            println!("GATE 4 FAIL — {f}");
        }
        Err("duplex soak failed".into())
    }
}
