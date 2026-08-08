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
//! * **Turn currency.** Exactly one [`TurnId`] is ever active. Late results
//!   for an abandoned turn (a stale [`Input::FinalTranscript`], a stale
//!   [`Input::AgentTextArrived`]) are dropped, never rendered. See
//!   [`SessionMachine::is_current`].
//! * **Pre-roll.** Barge-in confirmation costs real time, and those first
//!   frames are the start of whatever the user is saying. [`PreRollBuffer`]
//!   keeps them so a confirmed barge-in can replay the word that would
//!   otherwise be lost.
//!
//! Timeouts ([`IDLE_TIMEOUT`], [`AGENT_RESPONSE_TIMEOUT`], [`MAX_UTTERANCE`],
//! [`BARGE_IN_RETRACTION_WINDOW`]) are all driven purely by [`Input::Tick`]
//! -- nothing in this crate reads the wall clock, so every test is exactly
//! reproducible.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod machine;
mod preroll;
mod types;

pub use machine::SessionMachine;
pub use preroll::PreRollBuffer;
pub use types::{
    Frame, Input, Output, SessionState, TurnId, AGENT_RESPONSE_TIMEOUT, BARGE_IN_RETRACTION_WINDOW,
    IDLE_TIMEOUT, MAX_UTTERANCE, PREROLL_DURATION, PREROLL_FRAMES,
};
