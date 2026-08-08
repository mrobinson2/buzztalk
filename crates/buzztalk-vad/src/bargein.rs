//! Strict barge-in detector: interruption, not turn-taking.
//!
//! A false positive here cuts the agent off mid-sentence -- the failure mode
//! that makes users switch voice off permanently. Every gate in this module
//! exists to make that specific mistake hard:
//!
//! * a higher voicing threshold than [`crate::EndpointDetector`],
//! * fast-attack confirmation over a short sliding window rather than a
//!   single frame, so a single spike can't trigger it but real speech still
//!   confirms in tens of milliseconds,
//! * an ERLE gate: if the echo canceller isn't keeping up, anything "heard"
//!   on a speaker route is probably the agent's own voice leaking back in,
//! * a route gate: headphones have no acoustic loop, so the ERLE gate (and
//!   the convergence guard, which exists only to protect that same loop)
//!   don't apply,
//! * a convergence guard: right after playback starts the canceller hasn't
//!   adapted yet, so acoustic detection is suppressed for a short warm-up
//!   even if ERLE briefly looks fine,
//! * a spectral-variation check: a cough or a door slam is a broadband
//!   impulse with a near-static spectral shape from frame to frame; speech
//!   varies. This is checked unconditionally, on every route -- it isn't an
//!   echo-path gate, it's a "was that actually a voice" gate.

use std::collections::VecDeque;

use buzztalk_core::{AecStats, DetectorEvent, OutputRoute, Result, SpeechDetector};

use crate::frontend::{FrontEndConfig, VoicingFrontend};

/// Tunable parameters for [`BargeInDetector`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BargeInConfig {
    /// Front-end (energy/ZCR) configuration.
    pub frontend: FrontEndConfig,
    /// SNR above the noise floor, in dB, required for a frame to count as
    /// voiced. Higher than [`crate::EndpointConfig`]'s -- a quiet, ambiguous
    /// sound should never be allowed to interrupt.
    pub voicing_threshold_db: f32,
    /// Size of the confirmation sliding window, in frames.
    pub confirm_window: usize,
    /// Voiced frames required within the window to confirm speech (fast
    /// attack). Default 3 of 4 frames (~40 ms at the 10 ms frame cadence).
    pub confirm_count: usize,
    /// Minimum [`AecStats::erle_db`], in dB, for the canceller to be trusted.
    /// Below this, the canceller isn't keeping up and acoustic detection is
    /// suppressed on any route with an echo path.
    pub erle_floor_db: f32,
    /// Warm-up after playback starts during which acoustic detection is
    /// suppressed regardless of reported ERLE, because the canceller has not
    /// adapted yet. Default 15 frames (~150 ms).
    pub convergence_warmup_frames: u32,
    /// Minimum population variance of ZCR across the confirmation window
    /// required to accept a candidate as speech rather than a stationary
    /// broadband impulse (cough, click, door slam).
    pub min_spectral_variance: f32,
    /// Consecutive silent frames after confirmed speech before `SpeechEnd`
    /// fires. Shorter than the endpoint detector's hangover on purpose: once
    /// a barge-in has been confirmed and acted on, there's no reason to hold
    /// the "active" state open as long as an endpointed turn.
    pub hangover_frames: u32,
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            frontend: FrontEndConfig::default(),
            voicing_threshold_db: 15.0,
            confirm_window: 4,
            confirm_count: 3,
            erle_floor_db: 12.0,
            convergence_warmup_frames: 15,
            // Measured against this crate's synthetic signals (see
            // tests/detection.rs): a frequency-stepping "speech" signal
            // holds ~9.5e-6 steady-state ZCR variance across the
            // confirmation window; a stationary multi-tone "cough" sits at
            // ~0 (identical ZCR every frame, by construction). This value
            // sits comfortably below the former and far above the latter.
            // It is a cheap proxy, not a real spectral-flux measure --
            // retune against real coughs/speech before shipping.
            min_spectral_variance: 0.000_002,
            hangover_frames: 20,
        }
    }
}

/// Strict, echo-gated voice-activity detector for interrupting playback.
///
/// See the module docs for the full gating story and
/// [`buzztalk_core::SpeechDetector`] for the trait contract. Gating inputs
/// ([`AecStats`], [`OutputRoute`], playback-start notifications) are pushed
/// in via setters rather than `push_frame`'s signature, since that signature
/// is fixed by the shared trait.
pub struct BargeInDetector {
    config: BargeInConfig,
    frontend: VoicingFrontend,
    voiced_window: VecDeque<bool>,
    zcr_window: VecDeque<f32>,
    active: bool,
    hangover_silent_frames: u32,
    aec_stats: AecStats,
    route: OutputRoute,
    warmup_remaining: u32,
}

impl BargeInDetector {
    /// Build a detector with the given configuration.
    ///
    /// Before any gating input is supplied, the detector defaults to
    /// [`OutputRoute::Unknown`] (an assumed echo path) with no known ERLE --
    /// i.e. it will not fire acoustically until the caller supplies real
    /// route/ERLE information, or the route is set to
    /// [`OutputRoute::Headphones`]. This is deliberate fail-closed behaviour:
    /// an un-gated barge-in detector is exactly the false-positive risk this
    /// crate exists to prevent.
    pub fn new(config: BargeInConfig) -> Self {
        let frontend = VoicingFrontend::new(config.frontend);
        let cap = config.confirm_window.max(1);
        Self {
            config,
            frontend,
            voiced_window: VecDeque::with_capacity(cap),
            zcr_window: VecDeque::with_capacity(cap),
            active: false,
            hangover_silent_frames: 0,
            aec_stats: AecStats::default(),
            route: OutputRoute::Unknown,
            warmup_remaining: 0,
        }
    }

    /// Report the echo canceller's current health. Consulted by the ERLE
    /// gate on every subsequent frame until updated again.
    pub fn set_aec_stats(&mut self, stats: AecStats) {
        self.aec_stats = stats;
    }

    /// Report where playback is currently routed. Consulted by the route
    /// gate on every subsequent frame until updated again.
    pub fn set_output_route(&mut self, route: OutputRoute) {
        self.route = route;
    }

    /// Notify the detector that playback has just started, arming the
    /// convergence guard for [`BargeInConfig::convergence_warmup_frames`].
    pub fn notify_playback_started(&mut self) {
        self.warmup_remaining = self.config.convergence_warmup_frames;
    }

    /// Notify the detector that playback has stopped, clearing the
    /// convergence guard. With no far-end signal there is nothing for the
    /// canceller to have failed to converge on.
    pub fn notify_playback_stopped(&mut self) {
        self.warmup_remaining = 0;
    }

    /// Whether the ERLE gate, route gate, and convergence guard together
    /// currently permit a new confirmation.
    fn gate_open(&self) -> bool {
        if !self.route.has_echo_path() {
            // Headphones: no acoustic loop, so neither the ERLE gate nor the
            // convergence guard (which exists only to protect that loop)
            // apply. Relaxed path.
            return true;
        }
        let erle_ok = self
            .aec_stats
            .erle_db
            .is_some_and(|erle| erle >= self.config.erle_floor_db);
        let warmup_ok = self.warmup_remaining == 0;
        erle_ok && warmup_ok
    }

    /// Cheap spectral-variation proxy: population variance of ZCR across the
    /// confirmation window. Low variance means near-static spectral content
    /// frame to frame -- an impulse, not speech.
    fn spectral_variation_ok(&self) -> bool {
        if self.zcr_window.len() < self.config.confirm_window {
            return false;
        }
        let n = self.zcr_window.len() as f32;
        let mean: f32 = self.zcr_window.iter().sum::<f32>() / n;
        let variance: f32 = self
            .zcr_window
            .iter()
            .map(|z| (z - mean).powi(2))
            .sum::<f32>()
            / n;
        variance >= self.config.min_spectral_variance
    }

    /// Whether at least `confirm_count` of the last `confirm_window` frames
    /// were voiced.
    fn window_confirmed(&self) -> bool {
        self.voiced_window.len() >= self.config.confirm_window
            && self.voiced_window.iter().filter(|&&v| v).count() >= self.config.confirm_count
    }
}

fn push_capped<T>(buf: &mut VecDeque<T>, value: T, cap: usize) {
    if cap == 0 {
        return;
    }
    if buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(value);
}

impl SpeechDetector for BargeInDetector {
    fn push_frame(&mut self, frame: &[f32]) -> Result<DetectorEvent> {
        let features = self.frontend.process(frame)?;
        let voiced = features.snr_db >= self.config.voicing_threshold_db;

        if self.warmup_remaining > 0 {
            self.warmup_remaining -= 1;
        }

        push_capped(&mut self.voiced_window, voiced, self.config.confirm_window);
        // Only accumulate ZCR from voiced frames, and reset on a silent one.
        // Otherwise a leading silent frame's near-zero ZCR sits in the same
        // window as the first voiced frames of a genuine impulse and makes
        // it look spectrally *varied* right at the moment confirmation is
        // decided -- exactly backwards. This way the window always reflects
        // variation within the current voiced run, not across the onset
        // boundary.
        if voiced {
            push_capped(
                &mut self.zcr_window,
                features.zcr,
                self.config.confirm_window,
            );
        } else {
            self.zcr_window.clear();
        }

        let event = if self.active {
            if voiced {
                self.hangover_silent_frames = 0;
                DetectorEvent::SpeechContinue
            } else {
                self.hangover_silent_frames += 1;
                if self.hangover_silent_frames >= self.config.hangover_frames {
                    self.active = false;
                    self.hangover_silent_frames = 0;
                    DetectorEvent::SpeechEnd
                } else {
                    DetectorEvent::SpeechContinue
                }
            }
        } else if self.window_confirmed() && self.gate_open() && self.spectral_variation_ok() {
            self.active = true;
            self.hangover_silent_frames = 0;
            DetectorEvent::SpeechStart
        } else {
            DetectorEvent::Idle
        };

        Ok(event)
    }

    fn reset(&mut self) {
        self.frontend.reset();
        self.voiced_window.clear();
        self.zcr_window.clear();
        self.active = false;
        self.hangover_silent_frames = 0;
        self.warmup_remaining = 0;
        // aec_stats / route deliberately survive reset(): they describe the
        // outside world's current state (echo canceller health, playback
        // route), not detection history, and reset() is called on turn
        // boundaries where that state hasn't changed.
    }
}
