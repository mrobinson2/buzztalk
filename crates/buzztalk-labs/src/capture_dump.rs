//! Dump N wall-clock seconds of the duplex engine's capture path to a raw
//! 16-bit little-endian mono file (nominally 48 kHz), plus a one-line
//! verdict comparing wall-clock time against captured duration.
//!
//! The ratio is the point: if 5 wall seconds yield ~5 s of audio, the
//! device→48 kHz resample path is honest; if they yield 1.7 s or 15 s, the
//! resampler is mis-ratioed for the device's real rate and downstream STT
//! is being fed time-warped speech. Analyze the raw file with e.g.
//! `ffmpeg -f s16le -ar 48000 -ac 1 -i dump.raw -af volumedetect -f null -`.

use std::io::Write;
use std::time::{Duration, Instant};

use buzztalk_audio::{default_devices_signature, DuplexConfig, DuplexEngine};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/capture_dump.raw".to_string());

    let output_device = std::env::args().nth(3);
    println!("devices: {}", default_devices_signature());
    println!("output override: {output_device:?}");
    let mut engine = DuplexEngine::start(DuplexConfig {
        output_device,
        ..DuplexConfig::default()
    })
    .expect("engine start");
    let mut samples: Vec<f32> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        while let Some(frame) = engine.try_recv_capture() {
            samples.extend(frame);
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let mut file = std::fs::File::create(&out).expect("create output");
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in &samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    file.write_all(&bytes).expect("write output");

    let captured_secs = samples.len() as f64 / 48_000.0;
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    println!(
        "wall-clock {secs} s -> captured {:.2} s at nominal 48 kHz (ratio {:.2}), peak {:.3}, wrote {}",
        captured_secs,
        captured_secs / secs as f64,
        peak,
        out
    );
}
