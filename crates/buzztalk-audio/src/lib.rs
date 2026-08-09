//! Full-duplex audio engine for BuzzTalk, built on `cpal`.
//!
//! The one requirement that shapes everything here: whatever samples the
//! output callback actually hands to the device must also be published as
//! an echo-cancellation reference, sample for sample, including the
//! silence written when nothing is queued. That is why capture, playback,
//! and the render-reference tap all live behind one engine that owns both
//! cpal streams — a webview or a separate playback library can't give you
//! a *true* reference, because nothing outside the callback that wrote the
//! samples can know for certain what they were.
//!
//! ## Real-time discipline
//!
//! Both audio callbacks (`build_input_stream` / `build_output_stream`
//! closures below) only ever do: fixed-point arithmetic, atomic
//! `fetch_add`, and single-sample `try_push`/`try_pop` on a pre-allocated
//! [`ringbuf`] ring. No heap allocation, no locking, no logging, no
//! blocking. Framing raw samples into [`buzztalk_core::FRAME_SAMPLES`]
//! chunks and allocating the `Vec<f32>` handed back to callers happens
//! entirely on the consumer side, in [`framing::FrameReader`], which is
//! never touched by a callback.
//!
//! ## Sample rate and channel handling
//!
//! We always ask the device for [`SAMPLE_RATE_HZ`] (48 kHz) mono f32. If a
//! device can't do that — some USB interfaces are fixed at 44.1 kHz, some
//! multi-channel interfaces have no mono mode — we fall back to whatever
//! rate/channel count the device reports as its default-ish supported
//! range, fold multi-channel audio to mono by averaging channels, and
//! resample with [`resample::Resampler`], a small linear interpolator with
//! no anti-aliasing filter (see that module's docs for why that trade-off
//! is fine here). On this development machine (macOS arm64, built-in
//! input/output) the direct-48kHz path is what actually runs; the
//! resampling path is exercised by unit tests on synthetic data, not by a
//! real non-48kHz device — verify it against real hardware before
//! shipping if BuzzTalk needs to support fixed-44.1kHz interfaces.

#![warn(missing_docs)]

mod framing;
mod resample;
mod route;

pub use framing::FrameReader;
pub use route::detect_output_route;

use buzztalk_core::{Error, Result, FRAME_SAMPLES, SAMPLE_RATE_HZ};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfigRange,
};
use resample::Resampler;
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// How much audio each ring can hold before the real-time side starts
// dropping samples instead of blocking. Playback gets more headroom than
// capture/reference because it's fed in bursts (a whole TTS utterance at
// once) rather than at a steady drip.
const CAPTURE_RING_SECONDS: usize = 2;
const RENDER_REF_RING_SECONDS: usize = 2;
const PLAYBACK_RING_SECONDS: usize = 6;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for [`DuplexEngine::start`].
#[derive(Debug, Clone, Default)]
pub struct DuplexConfig {
    /// Exact device name to open for capture (as returned by
    /// [`enumerate_devices`]). `None` uses the host's default input device.
    pub input_device: Option<String>,
    /// Exact device name to open for playback. `None` uses the host's
    /// default output device.
    pub output_device: Option<String>,
    /// Requested buffer size, in frames (at whatever rate the device is
    /// opened at, not necessarily 48 kHz). `None` requests
    /// [`buzztalk_core::FRAME_SAMPLES`] and falls back to the device's
    /// default if that's outside its supported range or the range is
    /// unknown.
    pub preferred_buffer_frames: Option<u32>,
}

// ── Device enumeration ────────────────────────────────────────────────────────

/// Input and output device names available on the default host.
#[derive(Debug, Clone, Default)]
pub struct DeviceList {
    /// Names of devices that support audio input.
    pub input: Vec<String>,
    /// Names of devices that support audio output.
    pub output: Vec<String>,
}

/// List the input and output devices `cpal`'s default host can see.
pub fn enumerate_devices() -> Result<DeviceList> {
    let host = cpal::default_host();
    let input = host
        .input_devices()
        .map_err(|e| Error::Device(format!("enumerating input devices: {e}")))?
        .map(|d| d.to_string())
        .collect();
    let output = host
        .output_devices()
        .map_err(|e| Error::Device(format!("enumerating output devices: {e}")))?
        .map(|d| d.to_string())
        .collect();
    Ok(DeviceList { input, output })
}

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

fn find_device(
    host: &cpal::Host,
    wanted: Option<&str>,
    direction: Direction,
) -> Result<cpal::Device> {
    if let Some(name) = wanted {
        let mut devices = match direction {
            Direction::Input => host
                .input_devices()
                .map_err(|e| Error::Device(format!("listing input devices: {e}")))?
                .collect::<Vec<_>>(),
            Direction::Output => host
                .output_devices()
                .map_err(|e| Error::Device(format!("listing output devices: {e}")))?
                .collect::<Vec<_>>(),
        };
        let idx = devices
            .iter()
            .position(|d| d.to_string() == name)
            .ok_or_else(|| Error::Device(format!("no such device: {name:?}")))?;
        Ok(devices.swap_remove(idx))
    } else {
        match direction {
            Direction::Input => host
                .default_input_device()
                .ok_or_else(|| Error::Device("no default input device".into())),
            Direction::Output => host
                .default_output_device()
                .ok_or_else(|| Error::Device("no default output device".into())),
        }
    }
}

// ── Stream configuration negotiation ─────────────────────────────────────────

struct ChosenConfig {
    stream_config: StreamConfig,
    device_rate: u32,
}

/// Pick a stream config for `device`, preferring exactly [`SAMPLE_RATE_HZ`]
/// f32 at whatever channel count the device offers alongside it. Falls back
/// to the first f32-capable range the device advertises, at that range's
/// own rate, if 48 kHz isn't available anywhere — see the module docs for
/// how that fallback is handled downstream (channel folding + resampling).
fn choose_config(
    device: &cpal::Device,
    direction: Direction,
    preferred_buffer_frames: Option<u32>,
) -> Result<ChosenConfig> {
    let supported: Vec<SupportedStreamConfigRange> = match direction {
        Direction::Input => device
            .supported_input_configs()
            .map_err(|e| Error::Device(format!("querying supported input configs: {e}")))?
            .collect(),
        Direction::Output => device
            .supported_output_configs()
            .map_err(|e| Error::Device(format!("querying supported output configs: {e}")))?
            .collect(),
    };

    let native_rate = supported.iter().find(|r| {
        r.sample_format() == SampleFormat::F32
            && r.min_sample_rate() <= SAMPLE_RATE_HZ
            && SAMPLE_RATE_HZ <= r.max_sample_rate()
    });
    let (chosen, device_rate) = match native_rate {
        Some(r) => (*r, SAMPLE_RATE_HZ),
        None => {
            let r = supported
                .iter()
                .find(|r| r.sample_format() == SampleFormat::F32)
                .copied()
                .ok_or_else(|| Error::Device("device offers no f32 stream format".into()))?;
            // Devices overwhelmingly report a single fixed rate
            // (min == max) when they can't do 48 kHz; use it directly
            // rather than guessing at a "closer" bound.
            let rate = r.min_sample_rate();
            (r, rate)
        }
    };

    let buffer_size = match chosen.buffer_size() {
        SupportedBufferSize::Range { min, max } => {
            let want = preferred_buffer_frames.unwrap_or(FRAME_SAMPLES as u32);
            BufferSize::Fixed(want.clamp(*min, *max))
        }
        // The backend can't tell us a valid range up front; asking for a
        // fixed size here risks a hard failure on `build_*_stream`, so let
        // the device pick.
        SupportedBufferSize::Unknown => BufferSize::Default,
    };

    Ok(ChosenConfig {
        stream_config: StreamConfig {
            channels: chosen.channels(),
            sample_rate: device_rate,
            buffer_size,
        },
        device_rate,
    })
}

// ── Stats ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Counters {
    /// Raw samples dropped on the capture path because the ring was full
    /// (consumer not keeping up). Real-time safe: increment-and-drop, never
    /// block.
    capture_samples_dropped: AtomicU64,
    /// Raw samples dropped on the render-reference tap because that ring
    /// was full. If this is climbing, the AEC reference has gaps.
    render_ref_samples_dropped: AtomicU64,
    /// Times the output callback needed a playback sample and the playback
    /// ring was empty, so it substituted silence. Expected and harmless
    /// while nothing is queued; a steady stream of these *while audio is
    /// queued* means the playback producer isn't keeping up.
    playback_underrun_samples: AtomicU64,
    /// Samples passed to `push_playback` that didn't fit in the playback
    /// ring and were dropped (not written, not played).
    playback_samples_dropped: AtomicU64,
    /// Times either stream's error callback fired (device disconnected,
    /// default device changed, format renegotiated under us). Any non-zero
    /// value means this engine's streams can no longer be trusted and the
    /// owner should rebuild the engine — see
    /// [`DuplexEngine::stream_failed`].
    stream_errors: AtomicU64,
}

/// Snapshot of [`DuplexEngine`]'s overrun/underrun counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineStats {
    /// See [`Counters::capture_samples_dropped`].
    pub capture_samples_dropped: u64,
    /// See [`Counters::render_ref_samples_dropped`].
    pub render_ref_samples_dropped: u64,
    /// See [`Counters::playback_underrun_samples`].
    pub playback_underrun_samples: u64,
    /// See [`Counters::playback_samples_dropped`].
    pub playback_samples_dropped: u64,
}

#[inline(always)]
fn push_dropping(producer: &mut HeapProd<f32>, sample: f32, dropped: &AtomicU64) {
    if producer.try_push(sample).is_err() {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline(always)]
fn next_playback_sample(consumer: &mut HeapCons<f32>, underrun: &AtomicU64) -> f32 {
    use ringbuf::traits::Consumer;
    match consumer.try_pop() {
        Some(s) => s,
        None => {
            underrun.fetch_add(1, Ordering::Relaxed);
            0.0
        }
    }
}

// ── Playback handle ──────────────────────────────────────────────────────────

/// Producer side of the playback ring. Accepts arbitrary-length f32 PCM at
/// [`SAMPLE_RATE_HZ`] mono; the output callback drains it (resampling and
/// channel-duplicating to the device's actual format as needed).
pub struct PlaybackHandle {
    producer: HeapProd<f32>,
    counters: Arc<Counters>,
}

impl PlaybackHandle {
    fn new(producer: HeapProd<f32>, counters: Arc<Counters>) -> Self {
        Self { producer, counters }
    }

    /// Queue audio for playback. Never blocks: if the ring is full, the
    /// tail of `samples` that doesn't fit is dropped. Returns how many
    /// samples were dropped (0 means everything was queued).
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let pushed = self.producer.push_slice(samples);
        let dropped = samples.len() - pushed;
        if dropped > 0 {
            self.counters
                .playback_samples_dropped
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
        dropped
    }
}

// ── Engine ────────────────────────────────────────────────────────────────────

/// A running full-duplex audio engine: one cpal input stream, one cpal
/// output stream, and three lock-free rings connecting them to the rest of
/// the app.
///
/// Dropping this stops both streams (cpal tears them down when the
/// `Stream` values drop).
pub struct DuplexEngine {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    /// Framed 48 kHz mono microphone audio. `capture.try_recv()` (or the
    /// [`DuplexEngine::try_recv_capture`] shortcut) pops one
    /// [`buzztalk_core::FRAME_SAMPLES`] frame at a time.
    pub capture: FrameReader,
    /// Framed 48 kHz mono copy of exactly what was written to the output
    /// device — including silence when nothing was queued. This is the
    /// echo-cancellation reference.
    pub render_ref: FrameReader,
    /// Queue audio for playback. See [`DuplexEngine::push_playback`].
    pub playback: PlaybackHandle,
    counters: Arc<Counters>,
    output_latency_ms: Option<u32>,
}

impl DuplexEngine {
    /// Open an input and output stream per `config` and start them
    /// running. Both are opened as close to [`SAMPLE_RATE_HZ`] mono f32 as
    /// the device allows; see the module docs for the fallback when it
    /// doesn't.
    pub fn start(config: DuplexConfig) -> Result<Self> {
        let host = cpal::default_host();
        let input_device = find_device(&host, config.input_device.as_deref(), Direction::Input)?;
        let output_device = find_device(&host, config.output_device.as_deref(), Direction::Output)?;

        let input_chosen = choose_config(
            &input_device,
            Direction::Input,
            config.preferred_buffer_frames,
        )?;
        let output_chosen = choose_config(
            &output_device,
            Direction::Output,
            config.preferred_buffer_frames,
        )?;

        let capture_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * CAPTURE_RING_SECONDS);
        let (capture_prod, capture_cons) = capture_rb.split();
        let render_ref_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * RENDER_REF_RING_SECONDS);
        let (render_ref_prod, render_ref_cons) = render_ref_rb.split();
        let playback_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * PLAYBACK_RING_SECONDS);
        let (playback_prod, playback_cons) = playback_rb.split();

        let counters = Arc::new(Counters::default());

        let input_stream = build_input_stream(
            &input_device,
            &input_chosen,
            capture_prod,
            Arc::clone(&counters),
        )?;
        input_stream
            .play()
            .map_err(|e| Error::Device(format!("starting input stream: {e}")))?;

        let output_stream = build_output_stream(
            &output_device,
            &output_chosen,
            playback_cons,
            render_ref_prod,
            Arc::clone(&counters),
        )?;
        output_stream
            .play()
            .map_err(|e| Error::Device(format!("starting output stream: {e}")))?;

        let output_latency_ms = match output_chosen.stream_config.buffer_size {
            BufferSize::Fixed(frames) => {
                Some((frames as u64 * 1000 / output_chosen.device_rate.max(1) as u64) as u32)
            }
            BufferSize::Default => None,
        };

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            capture: FrameReader::new(capture_cons),
            render_ref: FrameReader::new(render_ref_cons),
            playback: PlaybackHandle::new(playback_prod, Arc::clone(&counters)),
            counters,
            output_latency_ms,
        })
    }

    /// Non-blocking pop of one 480-sample capture frame. `None` if a full
    /// frame isn't available yet.
    pub fn try_recv_capture(&mut self) -> Option<Vec<f32>> {
        self.capture.try_recv()
    }

    /// Non-blocking pop of one 480-sample frame of exactly what was
    /// written to the output device, including silence. `None` if a full
    /// frame isn't available yet.
    pub fn try_recv_render_ref(&mut self) -> Option<Vec<f32>> {
        self.render_ref.try_recv()
    }

    /// Queue arbitrary-length 48 kHz mono audio for playback. Never
    /// blocks; returns how many trailing samples were dropped because the
    /// playback ring was full.
    pub fn push_playback(&mut self, samples: &[f32]) -> usize {
        self.playback.push(samples)
    }

    /// Output latency in milliseconds, derived from the negotiated buffer
    /// size, for seeding AEC delay estimation. `None` if the device didn't
    /// negotiate a fixed buffer size ([`cpal::BufferSize::Default`]).
    pub fn output_latency_ms(&self) -> Option<u32> {
        self.output_latency_ms
    }

    /// True once either stream's error callback has fired (device
    /// disconnected, default route changed, format renegotiated). The
    /// streams may still appear to run, but their audio can no longer be
    /// trusted — Bluetooth headsets in particular renegotiate their format
    /// around profile switches and then feed a stale-rate stream
    /// time-warped audio. The owner should drop this engine and start a
    /// fresh one.
    pub fn stream_failed(&self) -> bool {
        self.counters.stream_errors.load(Ordering::Relaxed) > 0
    }

    /// Snapshot of overrun/underrun counters.
    pub fn stats(&self) -> EngineStats {
        EngineStats {
            capture_samples_dropped: self
                .counters
                .capture_samples_dropped
                .load(Ordering::Relaxed),
            render_ref_samples_dropped: self
                .counters
                .render_ref_samples_dropped
                .load(Ordering::Relaxed),
            playback_underrun_samples: self
                .counters
                .playback_underrun_samples
                .load(Ordering::Relaxed),
            playback_samples_dropped: self
                .counters
                .playback_samples_dropped
                .load(Ordering::Relaxed),
        }
    }
}

/// A cheap fingerprint of the host's current default input and output
/// devices: names plus their advertised default sample rates.
///
/// Bluetooth headsets renegotiate their format around profile switches
/// (A2DP <-> HFP, typically when playback starts or stops) *without*
/// firing a stream error — the stale stream keeps running and delivers
/// time-warped audio. Polling this signature and rebuilding the engine
/// when it changes is how an owner catches that silent renegotiation.
/// Costs a couple of CoreAudio property reads; fine at ~1 Hz.
pub fn default_devices_signature() -> String {
    let host = cpal::default_host();
    let describe_in = host
        .default_input_device()
        .map(|d| {
            let name = d.to_string();
            let rate = d
                .default_input_config()
                .map(|c| c.sample_rate())
                .unwrap_or(0);
            format!("{name}@{rate}")
        })
        .unwrap_or_else(|| "none".into());
    let describe_out = host
        .default_output_device()
        .map(|d| {
            let name = d.to_string();
            let rate = d
                .default_output_config()
                .map(|c| c.sample_rate())
                .unwrap_or(0);
            format!("{name}@{rate}")
        })
        .unwrap_or_else(|| "none".into());
    format!("in:{describe_in}|out:{describe_out}")
}

fn build_input_stream(
    device: &cpal::Device,
    chosen: &ChosenConfig,
    mut capture_producer: HeapProd<f32>,
    counters: Arc<Counters>,
) -> Result<cpal::Stream> {
    let channels = chosen.stream_config.channels as usize;
    let mut resampler = (chosen.device_rate != SAMPLE_RATE_HZ)
        .then(|| Resampler::new(chosen.device_rate, SAMPLE_RATE_HZ));
    let error_counters = Arc::clone(&counters);

    device
        .build_input_stream(
            chosen.stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if channels == 0 {
                    return;
                }
                for frame in data.chunks_exact(channels) {
                    let mono = frame.iter().sum::<f32>() / channels as f32;
                    match &mut resampler {
                        Some(r) => r.push(mono, |s| {
                            push_dropping(
                                &mut capture_producer,
                                s,
                                &counters.capture_samples_dropped,
                            )
                        }),
                        None => push_dropping(
                            &mut capture_producer,
                            mono,
                            &counters.capture_samples_dropped,
                        ),
                    }
                }
            },
            move |err| {
                error_counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("buzztalk-audio: input stream error: {err}");
            },
            None,
        )
        .map_err(|e| Error::Device(format!("building input stream: {e}")))
}

fn build_output_stream(
    device: &cpal::Device,
    chosen: &ChosenConfig,
    mut playback_consumer: HeapCons<f32>,
    mut render_ref_producer: HeapProd<f32>,
    counters: Arc<Counters>,
) -> Result<cpal::Stream> {
    let channels = chosen.stream_config.channels as usize;
    let needs_resample = chosen.device_rate != SAMPLE_RATE_HZ;
    let mut playback_resampler =
        needs_resample.then(|| Resampler::new(SAMPLE_RATE_HZ, chosen.device_rate));
    let mut render_ref_resampler =
        needs_resample.then(|| Resampler::new(chosen.device_rate, SAMPLE_RATE_HZ));
    let error_counters = Arc::clone(&counters);

    device
        .build_output_stream(
            chosen.stream_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if channels == 0 {
                    return;
                }
                for frame in data.chunks_exact_mut(channels) {
                    // Exactly what will be written to every channel of this
                    // device frame — computed once, then both played and
                    // tapped for the AEC reference, so the two can never
                    // drift apart.
                    let sample = match &mut playback_resampler {
                        Some(r) => r.pull(|| {
                            next_playback_sample(
                                &mut playback_consumer,
                                &counters.playback_underrun_samples,
                            )
                        }),
                        None => next_playback_sample(
                            &mut playback_consumer,
                            &counters.playback_underrun_samples,
                        ),
                    };
                    for ch in frame.iter_mut() {
                        *ch = sample;
                    }
                    match &mut render_ref_resampler {
                        Some(r) => r.push(sample, |s48| {
                            push_dropping(
                                &mut render_ref_producer,
                                s48,
                                &counters.render_ref_samples_dropped,
                            )
                        }),
                        None => push_dropping(
                            &mut render_ref_producer,
                            sample,
                            &counters.render_ref_samples_dropped,
                        ),
                    }
                }
            },
            move |err| {
                error_counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("buzztalk-audio: output stream error: {err}");
            },
            None,
        )
        .map_err(|e| Error::Device(format!("building output stream: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Consumer;

    /// The core silence-fill contract, exercised without a real device:
    /// drain an empty playback ring the way the output callback does, and
    /// confirm the render reference still gets a sample (0.0, plus an
    /// underrun tick) for every device frame instead of a gap.
    #[test]
    fn silent_output_still_publishes_to_render_reference() {
        let counters = Counters::default();
        let render_ref_rb = HeapRb::<f32>::new(FRAME_SAMPLES * 4);
        let (mut render_ref_prod, render_ref_cons) = render_ref_rb.split();
        let playback_rb = HeapRb::<f32>::new(FRAME_SAMPLES * 4);
        let (_playback_prod, mut playback_cons) = playback_rb.split();
        let mut reader = FrameReader::new(render_ref_cons);

        // Nothing was ever queued for playback: every pull is silence.
        for _ in 0..FRAME_SAMPLES {
            let sample =
                next_playback_sample(&mut playback_cons, &counters.playback_underrun_samples);
            assert_eq!(sample, 0.0);
            push_dropping(
                &mut render_ref_prod,
                sample,
                &counters.render_ref_samples_dropped,
            );
        }

        assert_eq!(
            counters.playback_underrun_samples.load(Ordering::Relaxed),
            FRAME_SAMPLES as u64
        );
        let frame = reader
            .try_recv()
            .expect("a full silent frame should be ready");
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().all(|&s| s == 0.0));
        assert_eq!(
            counters.render_ref_samples_dropped.load(Ordering::Relaxed),
            0
        );
    }

    /// The render reference must be *exactly* what was "played" (in this
    /// harness: pushed into the playback ring and drained the way the
    /// output callback drains it), sample for sample.
    #[test]
    fn render_reference_matches_playback_bit_exactly_at_native_rate() {
        let counters = Counters::default();
        let playback_rb = HeapRb::<f32>::new(FRAME_SAMPLES * 4);
        let (mut playback_prod, mut playback_cons) = playback_rb.split();
        let render_ref_rb = HeapRb::<f32>::new(FRAME_SAMPLES * 4);
        let (mut render_ref_prod, render_ref_cons) = render_ref_rb.split();
        let mut reader = FrameReader::new(render_ref_cons);

        let source: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| (i as f32 * 0.001).sin())
            .collect();
        assert_eq!(playback_prod.push_slice(&source), FRAME_SAMPLES);

        for _ in 0..FRAME_SAMPLES {
            let sample =
                next_playback_sample(&mut playback_cons, &counters.playback_underrun_samples);
            push_dropping(
                &mut render_ref_prod,
                sample,
                &counters.render_ref_samples_dropped,
            );
        }

        let frame = reader.try_recv().unwrap();
        assert_eq!(frame, source);
        assert_eq!(
            counters.playback_underrun_samples.load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn capture_ring_overrun_drops_and_counts_instead_of_blocking() {
        let counters = Counters::default();
        let rb = HeapRb::<f32>::new(4);
        let (mut prod, mut cons) = rb.split();

        for i in 0..10 {
            push_dropping(&mut prod, i as f32, &counters.capture_samples_dropped);
        }
        assert_eq!(counters.capture_samples_dropped.load(Ordering::Relaxed), 6);
        let mut drained = 0;
        while cons.try_pop().is_some() {
            drained += 1;
        }
        assert_eq!(drained, 4, "ring should hold exactly its capacity, no more");
    }

    #[test]
    fn playback_handle_reports_dropped_tail_on_overrun() {
        let counters = Arc::new(Counters::default());
        let rb = HeapRb::<f32>::new(8);
        let (prod, _cons) = rb.split();
        let mut handle = PlaybackHandle::new(prod, Arc::clone(&counters));

        let samples = vec![0.0_f32; 20];
        let dropped = handle.push(&samples);
        assert_eq!(dropped, 12);
        assert_eq!(
            counters.playback_samples_dropped.load(Ordering::Relaxed),
            12
        );
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = DuplexConfig::default();
        assert!(cfg.input_device.is_none());
        assert!(cfg.output_device.is_none());
        assert!(cfg.preferred_buffer_frames.is_none());
    }

    /// Real hardware required; not run in CI.
    #[test]
    #[ignore]
    fn start_engine_on_real_devices() {
        let engine = DuplexEngine::start(DuplexConfig::default()).expect("engine should start");
        assert!(engine.output_latency_ms().is_some() || engine.output_latency_ms().is_none());
    }

    /// Real hardware required; not run in CI.
    #[test]
    #[ignore]
    fn enumerate_real_devices() {
        let list = enumerate_devices().expect("should enumerate devices");
        println!("{list:?}");
    }

    /// Real hardware required; not run in CI (and meaningful only on
    /// macOS, though it should at least not panic elsewhere).
    #[test]
    #[ignore]
    fn detect_real_output_route() {
        let route = detect_output_route();
        println!("{route}");
    }
}
