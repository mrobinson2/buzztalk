//! `buzztalk-pipeline`: the integration layer that turns BuzzTalk's
//! standalone crates into an actual, interruptible conversation.
//!
//! Everything downstream of here already works in isolation: the duplex
//! engine has a bit-exact render reference, the echo canceller measures
//! real ERLE, the two voice-activity detectors are gated and tested, the
//! session machine's turn model is exhaustively unit tested, and STT/TTS
//! each run against real models. What was missing was the wiring. This
//! crate owns the threads, feeds `AecStats`/`OutputRoute` into the
//! barge-in gate every frame, and acts on every `Output` the session
//! machine emits -- including the two that are easiest to get wrong:
//! `CancelPlayback` (must make the speaker actually go quiet, not just
//! stop enqueueing more audio) and `ResumeSpeaking` (a retracted barge-in
//! must not lose a word of the agent's reply).
//!
//! # Threading
//!
//! Three threads, matching the poll-and-sleep discipline
//! `buzztalk-labs`'s harnesses already establish for the audio path, plus
//! one worker per CPU-bound model:
//!
//! * the **orchestrator** thread ([`ConversationPipeline::start`] spawns
//!   it) pulls capture/render-reference frames from
//!   [`buzztalk_audio::DuplexEngine`], runs AEC and both detectors, drives
//!   [`buzztalk_session::SessionMachine`], and feeds the playback ring --
//!   all cheap, allocation-light CPU work, same as the rest of this
//!   codebase's audio-frame loops;
//! * the **STT** thread owns a `ParakeetRecognizer`; the steady stream of
//!   per-frame audio reaches it over a bounded channel the orchestrator
//!   drops into (never blocks on) if the recognizer falls behind;
//! * the **TTS** thread owns a `PocketSynthesizer` the same way.
//!
//! The audio-frame loop itself never touches either model directly, and
//! never blocks on inference -- see [`stt_worker`] and [`tts_worker`] (both
//! private; the public surface is [`ConversationPipeline`]).
//!
//! # Why `CancelPlayback` doesn't need a "true" flush API
//!
//! `buzztalk-audio`'s `DuplexEngine`/`PlaybackHandle` expose only
//! `push_playback` -- there is no way to discard samples already sitting in
//! the playback ring once they've been handed over. This crate works
//! around that by never handing the engine more than a small, bounded
//! lookahead of audio at a time (`JIT_LEAD_FRAMES`, ~40 ms): the
//! orchestrator tops the ring up a few frames every tick instead of
//! pushing a whole utterance at once, so "cancel" reduces to "stop topping
//! up", and whatever's left in the ring drains in tens of milliseconds
//! instead of however long the utterance was. See the `playback` module
//! (private; its logic is exercised through `ConversationPipeline` and its
//! own unit tests) and this crate's implementation report for why adding a
//! real flush to `buzztalk-audio` would still be worth doing.

#![warn(missing_docs)]

mod agent;
mod audio_engine;
mod capture;
mod error;
mod metrics;
mod pipeline;
mod playback;
mod stt_worker;
mod tts_worker;

pub use agent::{AgentBackend, AgentEvent, EchoAgent};
pub use error::PipelineError;
pub use metrics::TurnMetrics;
pub use pipeline::{Capabilities, ConversationPipeline, PipelineConfig, PipelineEvent};

/// Re-exported so a caller building a [`PipelineConfig`] doesn't also need
/// a direct `buzztalk-audio` dependency just to name its type.
pub use buzztalk_audio::DuplexConfig;
/// Re-exported so a caller building a [`PipelineConfig`] doesn't also need
/// a direct `buzztalk-core` dependency just to force an
/// [`buzztalk_core::OutputRoute`].
pub use buzztalk_core::OutputRoute;

/// The default `--simulate` WAV fixture location, exposed for callers that
/// want the same default the demo binary uses.
pub fn default_simulate_wav() -> std::path::PathBuf {
    capture::default_simulate_wav()
}
