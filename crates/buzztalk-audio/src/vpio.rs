//! macOS `VoiceProcessingIO` full-duplex audio engine: an alternative to
//! [`crate::DuplexEngine`] that opens ONE `AudioUnit` covering both capture
//! and render instead of two independent `cpal` streams.
//!
//! ## Why this exists
//!
//! `DuplexEngine` opens a `cpal` input stream and a `cpal` output stream
//! independently. On a Bluetooth headset that starves the microphone to
//! digital silence: two independent CoreAudio clients each grabbing a half
//! of the same Bluetooth device don't coordinate the handset's HFP
//! (hands-free) profile the way a single client asking for both directions
//! at once does — see `docs/live-session-2026-08-09/SESSION-REPORT.md`
//! ("duplex-on-BT") for the measurement that pinned this down. Apple's
//! `kAudioUnitSubType_VoiceProcessingIO` audio unit is built for exactly
//! this: input (bus 1) and output (bus 0) live on one `AudioUnit` and one
//! underlying audio session, so CoreAudio negotiates Bluetooth correctly.
//! As a side effect it also runs Apple's own echo cancellation over the
//! captured signal — this engine doesn't depend on that (`buzztalk-aec`
//! still owns cancellation for the pipeline), but it's a free second layer.
//!
//! ## Real-time discipline
//!
//! Identical rule to [`crate::DuplexEngine`]'s two `cpal` callbacks (see
//! that module's docs): the render and input callbacks registered in
//! [`VoiceProcessingEngine::start`] only ever do fixed-point arithmetic,
//! atomic `fetch_add`, and single-sample `try_push`/`try_pop` on a
//! pre-allocated [`ringbuf`] ring. Everything that can allocate — the
//! `AudioUnit` itself, property negotiation, ring and resampler
//! construction — happens in `start`, before either callback is armed.
//!
//! ## v1 limitations
//!
//! * [`crate::DuplexConfig::input_device`] / `output_device` are ignored.
//!   `VoiceProcessingIO` is designed to follow the system's current
//!   default input/output route — which is exactly the behaviour this
//!   engine exists for on Bluetooth — and offers no supported way to pin
//!   it to a named device the way `cpal` can. If per-device selection is
//!   ever needed for VPIO, it would have to go through
//!   `kAudioOutputUnitProperty_CurrentDevice`, which Apple's own docs call
//!   out as unsupported for this audio unit subtype.
//! * [`VoiceProcessingEngine::output_latency_ms`] always returns `None` —
//!   see its docs for why.
//! * [`VoiceProcessingEngine::stream_failed`] cannot observe a real
//!   `AudioUnitRender` `OSStatus` the way `DuplexEngine::stream_failed`
//!   observes `cpal`'s per-stream error callback — see that method's docs.
//! * The 48 kHz-refused fallback path (mirroring `DuplexEngine`'s device
//!   fallback, see [`crate`]'s module docs) is implemented and unit tested
//!   against synthetic `StreamFormat` values, but has never been exercised
//!   against a real device that actually refuses 48 kHz mono —
//!   `VoiceProcessingIO`'s client-format property goes through CoreAudio's
//!   built-in converter and accepts arbitrary PCM formats in every case
//!   this crate's development hardware has hit. Verify against real
//!   hardware before relying on it if that ever changes.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use buzztalk_core::{Error, Result, SAMPLE_RATE_HZ};
use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use objc2_audio_toolbox::kAudioOutputUnitProperty_EnableIO;
use ringbuf::traits::Split;
use ringbuf::HeapRb;

use crate::resample::Resampler;
use crate::{
    next_playback_sample, push_dropping, Counters, DuplexConfig, EngineStats, FrameReader,
    PlaybackHandle, CAPTURE_RING_SECONDS, PLAYBACK_RING_SECONDS, RENDER_REF_RING_SECONDS,
};

/// The channel count and sample rate this engine will actually run one
/// side (input or output) of the audio unit at, decided once during
/// [`VoiceProcessingEngine::start`] by [`negotiate_side`].
struct NegotiatedSide {
    channels: usize,
    device_rate: u32,
}

/// The client stream format this engine always asks for first: f32,
/// interleaved (so a single [`data::Interleaved`] buffer covers any
/// channel count `VoiceProcessingIO` hands back, unlike
/// [`data::NonInterleaved`], which `coreaudio-rs`'s `set_input_callback`
/// only supports for exactly one channel — see that crate's own
/// `feedback.rs` example), mono, [`SAMPLE_RATE_HZ`].
fn desired_stream_format() -> StreamFormat {
    StreamFormat {
        sample_rate: SAMPLE_RATE_HZ as f64,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: 1,
    }
}

/// Pure fallback decision, split out of [`negotiate_side`] so it can be
/// unit tested without a real `AudioUnit`: given whatever `StreamFormat`
/// CoreAudio reports back after refusing [`desired_stream_format`], work
/// out the channel count and rate to actually run this side at.
///
/// Clamps both to sane minimums (at least 1 channel, at least 1 Hz) so a
/// degenerate all-zero `AudioStreamBasicDescription` — which CoreAudio
/// should never hand back, but nothing here trusts that — can't produce a
/// divide-by-zero downstream in the channel-folding or resampler math.
fn negotiated_side_from_format(actual: &StreamFormat) -> NegotiatedSide {
    NegotiatedSide {
        channels: (actual.channels as usize).max(1),
        device_rate: actual.sample_rate.round().max(1.0) as u32,
    }
}

/// Ask `audio_unit` for [`desired_stream_format`] on `scope`/`element`. If
/// it's refused, fall back to whatever format is already current there and
/// resample to/from [`SAMPLE_RATE_HZ`] downstream — the same fallback
/// this crate's `choose_config` performs for `cpal` devices, applied here to an
/// `AudioUnit`'s client format instead of a device's physical format. See
/// the module docs' "v1 limitations" for how well-exercised this path is.
fn negotiate_side(
    audio_unit: &mut AudioUnit,
    scope: Scope,
    element: Element,
    label: &str,
) -> Result<NegotiatedSide> {
    if audio_unit
        .set_stream_format(desired_stream_format(), scope, element)
        .is_ok()
    {
        return Ok(NegotiatedSide {
            channels: 1,
            device_rate: SAMPLE_RATE_HZ,
        });
    }
    let actual = audio_unit.stream_format(scope, element).map_err(|e| {
        Error::Device(format!(
            "VoiceProcessingIO {label} refused {SAMPLE_RATE_HZ} Hz mono, and its current \
             format could not be read either: {e}"
        ))
    })?;
    Ok(negotiated_side_from_format(&actual))
}

/// Average an interleaved audio frame's channels down to one mono sample —
/// the same fold this crate's `build_input_stream` performs for `cpal`
/// devices, split out here as a pure function so it's unit testable
/// without a real callback.
#[inline(always)]
fn fold_to_mono(frame: &[f32], channels: usize) -> f32 {
    frame.iter().sum::<f32>() / channels as f32
}

/// A running `VoiceProcessingIO` full-duplex audio engine: one `AudioUnit`
/// with both capture and render bound to it, and three lock-free rings
/// connecting it to the rest of the app — the same shape as
/// [`crate::DuplexEngine`], see that type's docs for the ring contract
/// (bit-exact render reference, drop-instead-of-block everywhere).
///
/// Dropping this stops the unit: `_audio_unit` is declared first so it
/// drops (and its callbacks stop firing) before the rings its callbacks
/// write into are torn down — `coreaudio`'s [`AudioUnit`] stops,
/// uninitializes, and disposes itself in its own `Drop` impl, mirroring
/// how [`crate::DuplexEngine`] relies on `cpal::Stream`'s `Drop`.
pub struct VoiceProcessingEngine {
    _audio_unit: AudioUnit,
    /// Framed 48 kHz mono microphone audio. See
    /// [`VoiceProcessingEngine::try_recv_capture`].
    pub capture: FrameReader,
    /// Framed 48 kHz mono copy of exactly what was written to the output
    /// bus, including silence — the echo-cancellation reference. See
    /// [`VoiceProcessingEngine::try_recv_render_ref`].
    pub render_ref: FrameReader,
    /// Queue audio for playback. See [`VoiceProcessingEngine::push_playback`].
    pub playback: PlaybackHandle,
    counters: Arc<Counters>,
}

impl VoiceProcessingEngine {
    /// Create and start a `VoiceProcessingIO` audio unit covering both
    /// capture and render.
    ///
    /// `config`'s device fields are ignored — see the module docs' "v1
    /// limitations". Everything that can allocate (the unit itself, both
    /// rings, both callbacks' resamplers) happens here, before
    /// [`AudioUnit::start`] arms either callback.
    pub fn start(config: DuplexConfig) -> Result<Self> {
        // Explicit device selection isn't supported for VoiceProcessingIO
        // in this version — see the module docs. Named so a reviewer can
        // see at a glance that this isn't an oversight.
        let DuplexConfig {
            input_device: _ignored_input_device,
            output_device: _ignored_output_device,
            preferred_buffer_frames: _ignored_preferred_buffer_frames,
        } = config;

        let mut audio_unit = AudioUnit::new_uninitialized(IOType::VoiceProcessingIO)
            .map_err(|e| Error::Device(format!("creating VoiceProcessingIO audio unit: {e}")))?;

        // Output (bus 0) is enabled by default on an I/O unit; input
        // (bus 1) is not — ask for it explicitly, before initializing:
        // I/O enable state can't be changed on an already-initialized
        // unit.
        let enable_input: u32 = 1;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Input,
                Element::Input,
                Some(&enable_input),
            )
            .map_err(|e| Error::Device(format!("enabling VoiceProcessingIO input bus: {e}")))?;

        // Client format for the output bus (0) lives on the unit's Input
        // scope; for the input bus (1) it lives on the unit's Output
        // scope — the unit's own scope names describe what IT does with
        // the bus, not which direction audio flows for the app. Both must
        // be set before `initialize()`, same reasoning as enabling IO.
        let output_side = negotiate_side(&mut audio_unit, Scope::Input, Element::Output, "output")?;
        let input_side = negotiate_side(&mut audio_unit, Scope::Output, Element::Input, "input")?;

        audio_unit
            .initialize()
            .map_err(|e| Error::Device(format!("initializing VoiceProcessingIO: {e}")))?;

        let capture_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * CAPTURE_RING_SECONDS);
        let (mut capture_prod, capture_cons) = capture_rb.split();
        let render_ref_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * RENDER_REF_RING_SECONDS);
        let (mut render_ref_prod, render_ref_cons) = render_ref_rb.split();
        let playback_rb = HeapRb::<f32>::new(SAMPLE_RATE_HZ as usize * PLAYBACK_RING_SECONDS);
        let (playback_prod, mut playback_cons) = playback_rb.split();

        let counters = Arc::new(Counters::default());

        // ── Render callback (bus 0) ─────────────────────────────────────
        {
            let channels = output_side.channels;
            let needs_resample = output_side.device_rate != SAMPLE_RATE_HZ;
            let mut playback_resampler =
                needs_resample.then(|| Resampler::new(SAMPLE_RATE_HZ, output_side.device_rate));
            let mut render_ref_resampler =
                needs_resample.then(|| Resampler::new(output_side.device_rate, SAMPLE_RATE_HZ));
            let counters = Arc::clone(&counters);
            type OutArgs = render_callback::Args<data::Interleaved<f32>>;

            audio_unit
                .set_render_callback(move |args: OutArgs| {
                    let render_callback::Args { data, .. } = args;
                    if channels == 0 || data.channels != channels {
                        // The format negotiated at `start` no longer
                        // matches what CoreAudio is actually handing the
                        // callback (e.g. a route change renegotiated the
                        // unit mid-stream). There is no per-callback
                        // `OSStatus` this crate's safe wrapper surfaces
                        // for that (see `stream_failed`'s docs) — this is
                        // how such a mismatch actually becomes visible.
                        // Leave the device buffer untouched (silence)
                        // rather than mis-align samples into the rings.
                        counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                    for frame in data.buffer.chunks_exact_mut(channels) {
                        // Exactly what will be written to every channel —
                        // computed once, then both played and tapped for
                        // the AEC reference, so the two can never drift
                        // apart. Mirrors this crate's `build_output_stream`.
                        let sample = match &mut playback_resampler {
                            Some(r) => r.pull(|| {
                                next_playback_sample(
                                    &mut playback_cons,
                                    &counters.playback_underrun_samples,
                                )
                            }),
                            None => next_playback_sample(
                                &mut playback_cons,
                                &counters.playback_underrun_samples,
                            ),
                        };
                        for ch in frame.iter_mut() {
                            *ch = sample;
                        }
                        match &mut render_ref_resampler {
                            Some(r) => r.push(sample, |s48| {
                                push_dropping(
                                    &mut render_ref_prod,
                                    s48,
                                    &counters.render_ref_samples_dropped,
                                )
                            }),
                            None => push_dropping(
                                &mut render_ref_prod,
                                sample,
                                &counters.render_ref_samples_dropped,
                            ),
                        }
                    }
                    Ok(())
                })
                .map_err(|e| {
                    Error::Device(format!("setting VoiceProcessingIO render callback: {e}"))
                })?;
        }

        // ── Input callback (bus 1) ──────────────────────────────────────
        {
            let channels = input_side.channels;
            let needs_resample = input_side.device_rate != SAMPLE_RATE_HZ;
            let mut resampler =
                needs_resample.then(|| Resampler::new(input_side.device_rate, SAMPLE_RATE_HZ));
            let counters = Arc::clone(&counters);
            type InArgs = render_callback::Args<data::Interleaved<f32>>;

            audio_unit
                .set_input_callback(move |args: InArgs| {
                    let render_callback::Args { data, .. } = args;
                    if channels == 0 || data.channels != channels {
                        counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                    for frame in data.buffer.chunks_exact(channels) {
                        let mono = fold_to_mono(frame, channels);
                        match &mut resampler {
                            Some(r) => r.push(mono, |s| {
                                push_dropping(
                                    &mut capture_prod,
                                    s,
                                    &counters.capture_samples_dropped,
                                )
                            }),
                            None => push_dropping(
                                &mut capture_prod,
                                mono,
                                &counters.capture_samples_dropped,
                            ),
                        }
                    }
                    Ok(())
                })
                .map_err(|e| {
                    Error::Device(format!("setting VoiceProcessingIO input callback: {e}"))
                })?;
        }

        audio_unit
            .start()
            .map_err(|e| Error::Device(format!("starting VoiceProcessingIO: {e}")))?;

        Ok(Self {
            _audio_unit: audio_unit,
            capture: FrameReader::new(capture_cons),
            render_ref: FrameReader::new(render_ref_cons),
            playback: PlaybackHandle::new(playback_prod, Arc::clone(&counters)),
            counters,
        })
    }

    /// Non-blocking pop of one 480-sample capture frame. `None` if a full
    /// frame isn't available yet.
    pub fn try_recv_capture(&mut self) -> Option<Vec<f32>> {
        self.capture.try_recv()
    }

    /// Non-blocking pop of one 480-sample frame of exactly what was
    /// written to the output bus, including silence. `None` if a full
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

    /// Always `None`: unlike `cpal`'s device negotiation, `AudioUnit`
    /// exposes no fixed buffer-size property this engine can read at
    /// `start` time — `VoiceProcessingIO`'s callback buffer size is
    /// chosen by CoreAudio per-callback and can vary while running, so
    /// there is no single number to seed AEC delay estimation with the
    /// way [`crate::DuplexEngine::output_latency_ms`] does.
    pub fn output_latency_ms(&self) -> Option<u32> {
        None
    }

    /// True once a render or input callback has observed its buffer no
    /// longer matching the format negotiated at [`VoiceProcessingEngine::start`].
    ///
    /// Unlike [`crate::DuplexEngine::stream_failed`], this is not backed by
    /// a real CoreAudio error callback: `VoiceProcessingIO` has none, and
    /// `coreaudio-rs`'s safe `set_input_callback` wrapper calls
    /// `AudioUnitRender` internally and returns its `OSStatus` straight to
    /// the OS on failure *without* ever invoking the closure passed to it
    /// — so a real `AudioUnitRender` error is never observable from here.
    /// What this flag actually catches is the one failure mode this
    /// engine's own callbacks can see: the negotiated channel count no
    /// longer matching what CoreAudio hands the callback (e.g. a live
    /// route change). Treat a `false` here as "no *detected* failure," not
    /// as a guarantee the underlying unit is healthy — same caveat
    /// `DuplexEngine`'s device watchdog already lives with for silent
    /// Bluetooth renegotiation, just one layer further down.
    pub fn stream_failed(&self) -> bool {
        self.counters.stream_errors.load(Ordering::Relaxed) > 0
    }

    /// Snapshot of overrun/underrun counters — see [`EngineStats`].
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The format this engine always asks for first must actually match
    /// the `Data` type its callbacks are registered with — otherwise
    /// `set_render_callback`/`set_input_callback` would reject it at
    /// `start` time on every machine, real hardware or not. Checked here
    /// against `coreaudio-rs`'s own `Data::does_stream_format_match`
    /// instead of asserting on the flags directly, so this breaks loudly
    /// if either side of that contract drifts.
    #[test]
    fn desired_format_matches_interleaved_f32_data() {
        use coreaudio::audio_unit::render_callback::data::{Data, Interleaved};
        assert!(Interleaved::<f32>::does_stream_format_match(
            &desired_stream_format()
        ));
    }

    #[test]
    fn negotiated_side_from_format_clamps_channels_and_rate_to_at_least_one() {
        let degenerate = StreamFormat {
            sample_rate: 0.0,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: 0,
        };
        let side = negotiated_side_from_format(&degenerate);
        assert_eq!(side.channels, 1);
        assert_eq!(side.device_rate, 1);
    }

    #[test]
    fn negotiated_side_from_format_rounds_and_preserves_a_real_fallback_format() {
        let fallback = StreamFormat {
            sample_rate: 44_100.0,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: 2,
        };
        let side = negotiated_side_from_format(&fallback);
        assert_eq!(side.channels, 2);
        assert_eq!(side.device_rate, 44_100);
    }

    #[test]
    fn fold_to_mono_averages_channels() {
        assert_eq!(fold_to_mono(&[1.0, 3.0], 2), 2.0);
        assert_eq!(fold_to_mono(&[0.5], 1), 0.5);
        assert_eq!(fold_to_mono(&[1.0, 2.0, 3.0, 4.0], 4), 2.5);
    }

    /// Real hardware required; not run in CI.
    #[test]
    #[ignore]
    fn start_engine_on_real_devices() {
        let engine =
            VoiceProcessingEngine::start(DuplexConfig::default()).expect("engine should start");
        assert!(engine.output_latency_ms().is_none());
    }
}
