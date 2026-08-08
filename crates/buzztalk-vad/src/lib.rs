//! Deterministic voice-activity detection for BuzzTalk.
//!
//! Endpointing and barge-in have opposite error costs and cannot share a
//! threshold:
//!
//! * endpointing false negative = a lost word. Annoying.
//! * barge-in false positive = the agent gets cut off by a cough or residual
//!   echo. Infuriating, and it is the failure that makes users switch voice
//!   off permanently.
//!
//! This crate ships two [`buzztalk_core::SpeechDetector`] implementations
//! over one shared front-end instead of one detector with one threshold:
//!
//! * [`EndpointDetector`] -- permissive, long hangover, only guards against
//!   noise blips too short to be a real utterance.
//! * [`BargeInDetector`] -- strict fast-attack confirmation, a higher
//!   voicing threshold, and acoustic gating ([`buzztalk_core::AecStats`]'s
//!   ERLE, [`buzztalk_core::OutputRoute`], a post-playback convergence
//!   guard, and a spectral-variation check) that a plain threshold can't
//!   express.
//!
//! Both are pure Rust with no model files and no ONNX dependency: this is
//! the deterministic baseline the gating story is tested against, and a
//! neural VAD would make the gate tests non-deterministic.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bargein;
mod endpoint;
mod frontend;

pub use bargein::{BargeInConfig, BargeInDetector};
pub use endpoint::{EndpointConfig, EndpointDetector};
pub use frontend::{FrameFeatures, FrontEndConfig, VoicingFrontend};
