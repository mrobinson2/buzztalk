//! The conversation state machine and turn model for BuzzTalk.
//!
//! This is the product: everything else in the workspace (audio I/O, AEC,
//! VAD, STT) is plumbing that produces [`Input`]s for the machine defined
//! here, and executes the [`Output`]s it returns.
//!
//! # Design
//!
//! This crate depends on **only** `buzztalk-core`. It never imports an
//! audio device, an STT/TTS engine, or a VAD backend -- it is driven
//! entirely by injected [`Input`]s and produces only [`Output`]s describing
//! what the caller should do. That is what lets the same [`SessionMachine`]
//! serve both the desktop path (mic + speaker, `cpal`) and a future
//! telephony path (a completely different transport) unchanged.
//!
//! Two properties matter most:
//!
//! * **Turn currency, enforced by identity.** Exactly one [`TurnId`] is
//!   ever active. Every [`Input`] that reports an asynchronous result
//!   ([`Input::PartialTranscript`], [`Input::FinalTranscript`],
//!   [`Input::SubmitSucceeded`], [`Input::SubmitFailed`],
//!   [`Input::AgentTextArrived`], [`Input::AgentTurnComplete`],
//!   [`Input::PlaybackDrained`]) carries the [`TurnId`] it belongs to, and
//!   [`SessionMachine::handle`] checks that turn against
//!   [`SessionMachine::is_current`] **before** any state-dependent logic
//!   runs, rejecting it outright if it doesn't match. State gating is kept
//!   as a second, independent check behind that -- belt and braces. This is
//!   what makes a late result for an abandoned turn (e.g. a slow STT
//!   re-decode for a turn a barge-in already cancelled) impossible to
//!   mistake for a result of whatever turn is current now, even when both
//!   turns pass through the exact same [`SessionState`] at different
//!   moments.
//! * **Pre-roll.** Barge-in confirmation costs real time, and those first
//!   frames are the start of whatever the user is saying. [`PreRollBuffer`]
//!   keeps them so a confirmed barge-in can replay the word that would
//!   otherwise be lost. The [`TurnId`] for the barge-in candidate is
//!   allocated at the moment the barge-in is confirmed (not when it's later
//!   proven genuine) and handed to the caller via
//!   [`Output::ReplayPreRoll`], so the STT stream the caller starts over
//!   that replayed audio can be tagged correctly from its very first frame.
//!
//! Timeouts ([`IDLE_TIMEOUT`], [`AGENT_RESPONSE_TIMEOUT`], [`MAX_UTTERANCE`],
//! [`FINALIZE_TIMEOUT`], [`BARGE_IN_RETRACTION_WINDOW`]) are all driven
//! purely by [`Input::Tick`] -- nothing in this crate reads the wall clock,
//! so every test is exactly reproducible.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod machine;
mod preroll;
mod types;

pub use machine::SessionMachine;
pub use preroll::PreRollBuffer;
pub use types::{
    Frame, Input, Output, SessionState, TurnId, AGENT_RESPONSE_TIMEOUT, BARGE_IN_RETRACTION_WINDOW,
    FINALIZE_TIMEOUT, IDLE_TIMEOUT, MAX_UTTERANCE, PREROLL_DURATION, PREROLL_FRAMES,
};
