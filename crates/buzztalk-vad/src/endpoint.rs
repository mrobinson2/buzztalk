//! Permissive endpoint detector: turn-taking, not interruption.
//!
//! A false negative here loses a word, so this detector is tuned to be
//! forgiving -- a moderate voicing threshold, a long hangover before
//! declaring the speaker done, and no acoustic gating at all (endpointing is
//! about the user's own turn, not about the agent's playback).
//!
//! The one thing it *does* guard against is noise: a minimum sustained
//! voiced duration before an utterance counts as real, so a cough or a click
//! never opens an STT turn and becomes hallucinated transcript text.

use buzztalk_core::{DetectorEvent, Result, SpeechDetector};

use crate::frontend::{FrontEndConfig, VoicingFrontend};

/// Tunable parameters for [`EndpointDetector`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndpointConfig {
    /// Front-end (energy/ZCR) configuration.
    pub frontend: FrontEndConfig,
    /// SNR above the noise floor, in dB, required for a frame to count as
    /// voiced. Deliberately lower than [`crate::BargeInConfig`]'s -- missing
    /// a quiet word is a minor annoyance, not a product-breaking one.
    pub voicing_threshold_db: f32,
    /// Consecutive voiced frames required before `SpeechStart` fires.
    /// Default 19 frames (~190 ms) -- mirrors a guard the upstream Buzz code
    /// found necessary to stop noise becoming hallucinated transcript text.
    pub min_voiced_frames: u32,
    /// Consecutive silent frames required after voicing stops before
    /// `SpeechEnd` fires. Default 30 frames (~300 ms).
    pub hangover_frames: u32,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            frontend: FrontEndConfig::default(),
            voicing_threshold_db: 9.0,
            min_voiced_frames: 19,
            hangover_frames: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Silence,
    Confirming { voiced_frames: u32 },
    Active,
    Hangover { silent_frames: u32 },
}

/// Permissive voice-activity detector for turn-taking (endpointing).
///
/// See the module docs for the tuning rationale, and
/// [`buzztalk_core::SpeechDetector`] for the trait contract.
pub struct EndpointDetector {
    config: EndpointConfig,
    frontend: VoicingFrontend,
    state: State,
}

impl EndpointDetector {
    /// Build a detector with the given configuration.
    pub fn new(config: EndpointConfig) -> Self {
        let frontend = VoicingFrontend::new(config.frontend);
        Self {
            config,
            frontend,
            state: State::Silence,
        }
    }
}

impl Default for EndpointDetector {
    fn default() -> Self {
        Self::new(EndpointConfig::default())
    }
}

impl SpeechDetector for EndpointDetector {
    fn push_frame(&mut self, frame: &[f32]) -> Result<DetectorEvent> {
        let features = self.frontend.process(frame)?;
        let voiced = features.snr_db >= self.config.voicing_threshold_db;

        let event = match self.state {
            State::Silence => {
                if voiced {
                    if self.config.min_voiced_frames <= 1 {
                        self.state = State::Active;
                        DetectorEvent::SpeechStart
                    } else {
                        self.state = State::Confirming { voiced_frames: 1 };
                        DetectorEvent::Idle
                    }
                } else {
                    DetectorEvent::Idle
                }
            }
            State::Confirming { voiced_frames } => {
                if voiced {
                    let count = voiced_frames + 1;
                    if count >= self.config.min_voiced_frames {
                        self.state = State::Active;
                        DetectorEvent::SpeechStart
                    } else {
                        self.state = State::Confirming {
                            voiced_frames: count,
                        };
                        DetectorEvent::Idle
                    }
                } else {
                    // A short blip: voicing dropped out before the minimum
                    // sustained duration was reached. Discard it entirely
                    // rather than carrying a partial count forward.
                    self.state = State::Silence;
                    DetectorEvent::Idle
                }
            }
            State::Active => {
                if voiced {
                    DetectorEvent::SpeechContinue
                } else {
                    self.state = State::Hangover { silent_frames: 1 };
                    DetectorEvent::SpeechContinue
                }
            }
            State::Hangover { silent_frames } => {
                if voiced {
                    self.state = State::Active;
                    DetectorEvent::SpeechContinue
                } else if silent_frames + 1 >= self.config.hangover_frames {
                    self.state = State::Silence;
                    DetectorEvent::SpeechEnd
                } else {
                    self.state = State::Hangover {
                        silent_frames: silent_frames + 1,
                    };
                    DetectorEvent::SpeechContinue
                }
            }
        };

        Ok(event)
    }

    fn reset(&mut self) {
        self.frontend.reset();
        self.state = State::Silence;
    }
}
