//! Core types and traits for BuzzTalk.
//!
//! This crate has **no I/O and no engine dependencies**. If it ever imports
//! `cpal`, `sherpa-onnx`, or an echo-cancellation backend, the abstraction has
//! broken and the dependency belongs in a sibling crate instead.
//!
//! Everything here is shared by the desktop path and the (future) telephony
//! path. The rule that keeps telephony cheap: a transport produces and consumes
//! [`Frame`]-shaped audio, and nothing above this layer knows where it came
//! from.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

// ── Audio format ──────────────────────────────────────────────────────────────

/// Internal sample rate for the entire pipeline, in Hz.
///
/// Chosen to match what the microphone is opened at, Opus's native rate, and
/// the existing resampler stage in Buzz. Resampling happens only at engine
/// edges (STT wants 16 kHz, Pocket TTS emits 24 kHz, telephony is 8 kHz).
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Scheduling quantum for the whole system, in milliseconds.
///
/// 10 ms is what AEC3 wants, what Buzz's huddle playout loop already ticks at,
/// and what its TTS barge-in monitor polls at. One cadence everywhere.
pub const FRAME_MS: u32 = 10;

/// Samples in one mono frame at [`SAMPLE_RATE_HZ`] and [`FRAME_MS`].
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as usize * FRAME_MS as usize) / 1000;

/// Channel count. Mono throughout — stereo buys nothing for speech and doubles
/// every buffer.
pub const CHANNELS: usize = 1;

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors surfaced by BuzzTalk components.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A frame was not exactly [`FRAME_SAMPLES`] long.
    #[error("expected a {FRAME_SAMPLES}-sample frame, got {0}")]
    FrameLength(usize),

    /// An audio device could not be opened or configured.
    #[error("audio device: {0}")]
    Device(String),

    /// An echo-cancellation backend failed to initialise or process.
    #[error("echo canceller: {0}")]
    Aec(String),

    /// The component was used after it was shut down.
    #[error("component is shut down")]
    ShutDown,
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Validate that a slice is exactly one frame long.
pub fn check_frame(samples: &[f32]) -> Result<()> {
    if samples.len() == FRAME_SAMPLES {
        Ok(())
    } else {
        Err(Error::FrameLength(samples.len()))
    }
}

// ── Output routing ────────────────────────────────────────────────────────────

/// Where playback is physically going.
///
/// This is the highest-leverage signal in the whole system: on headphones there
/// is no acoustic loop from speaker back to microphone, so barge-in needs no
/// echo cancellation at all and every gate can be relaxed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRoute {
    /// Wired or Bluetooth headphones — no acoustic loop.
    Headphones,
    /// Loudspeakers — an acoustic loop exists and AEC is required.
    Speakers,
    /// Could not be determined. Treat as [`OutputRoute::Speakers`]: assuming an
    /// echo path that isn't there costs a little barge-in latency, while
    /// assuming one that is there costs false interruptions.
    Unknown,
}

impl OutputRoute {
    /// Whether an acoustic echo path should be assumed.
    pub fn has_echo_path(self) -> bool {
        !matches!(self, OutputRoute::Headphones)
    }
}

impl fmt::Display for OutputRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            OutputRoute::Headphones => "headphones",
            OutputRoute::Speakers => "speakers",
            OutputRoute::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

// ── Echo cancellation ─────────────────────────────────────────────────────────

/// Health of the echo canceller, published so the UI can tell the user when
/// their room is fighting them rather than silently misbehaving.
#[derive(Debug, Clone, Copy, Default)]
pub struct AecStats {
    /// Echo return loss enhancement, in dB. Higher is better; below ~12 dB the
    /// canceller is not keeping up and acoustic barge-in should be gated off.
    pub erle_db: Option<f32>,
    /// Estimated delay between the render reference and the captured echo, ms.
    pub estimated_delay_ms: Option<u32>,
    /// Whether the backend reports the near-end and far-end are both active.
    pub double_talk: bool,
}

/// An acoustic echo canceller.
///
/// Contract, and the ordering matters:
///
/// 1. [`process_render`](EchoCanceller::process_render) is called with the
///    **exact samples handed to the output device**, before or as they play.
/// 2. [`process_capture`](EchoCanceller::process_capture) is called with the
///    microphone frame, and cleans it in place.
///
/// Getting a *true* render reference is the entire reason BuzzTalk owns the
/// audio device instead of leaving capture in a webview.
pub trait EchoCanceller: Send {
    /// Feed the far-end (playback) reference frame.
    fn process_render(&mut self, far_end: &[f32]) -> Result<()>;

    /// Clean the near-end (microphone) frame in place.
    fn process_capture(&mut self, near_end: &mut [f32]) -> Result<()>;

    /// Tell the canceller how far the render reference lags the capture, in ms.
    /// Seeded from the device's reported latency and refined at runtime.
    fn set_stream_delay_ms(&mut self, delay_ms: u32);

    /// Current health. Used for gating, and shown to the user.
    fn stats(&self) -> AecStats;

    /// Backend identifier, for logs and the bake-off report.
    fn name(&self) -> &'static str;
}

// ── Speech detection ──────────────────────────────────────────────────────────

/// What a detector concluded about the frame it was just given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorEvent {
    /// Nothing of interest.
    Idle,
    /// Speech started (confirmation criteria met).
    SpeechStart,
    /// Speech is ongoing.
    SpeechContinue,
    /// Speech ended (hangover elapsed).
    SpeechEnd,
}

/// A voice-activity detector.
///
/// BuzzTalk runs **two** implementations of this on the same cleaned stream,
/// because endpointing and barge-in have opposite error costs and cannot share
/// a threshold:
///
/// * endpointing — a false negative loses a word; tuned permissive, ~300 ms
///   hangover.
/// * barge-in — a false positive cuts the agent off mid-sentence; tuned strict,
///   fast attack, and additionally gated on [`AecStats::erle_db`] and
///   [`OutputRoute`].
pub trait SpeechDetector: Send {
    /// Push exactly one [`FRAME_SAMPLES`] frame and get the resulting event.
    fn push_frame(&mut self, frame: &[f32]) -> Result<DetectorEvent>;

    /// Reset all internal state (called on turn boundaries and device changes).
    fn reset(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_geometry_is_consistent() {
        assert_eq!(FRAME_SAMPLES, 480);
        assert_eq!(
            FRAME_SAMPLES as u32 * 1000 / FRAME_MS,
            SAMPLE_RATE_HZ,
            "frame length must divide the sample rate evenly"
        );
    }

    #[test]
    fn check_frame_rejects_wrong_lengths() {
        assert!(check_frame(&vec![0.0; FRAME_SAMPLES]).is_ok());
        assert!(check_frame(&vec![0.0; FRAME_SAMPLES - 1]).is_err());
        assert!(check_frame(&[]).is_err());
    }

    #[test]
    fn unknown_route_assumes_an_echo_path() {
        assert!(OutputRoute::Unknown.has_echo_path());
        assert!(OutputRoute::Speakers.has_echo_path());
        assert!(!OutputRoute::Headphones.has_echo_path());
    }
}
