//! Shared types: state, turn identity, frames, timing constants, and the
//! input/output vocabulary the state machine speaks.

use std::fmt;
use std::time::Duration;

use buzztalk_core::DetectorEvent;

// ── Audio framing ───────────────────────────────────────────────────────────

/// One frame of audio samples, matching [`buzztalk_core::FRAME_SAMPLES`].
///
/// This crate never opens a device or an engine -- it only ever sees frames
/// that a caller hands it, so it can be driven identically from a live mic
/// or from a telephony trunk.
pub type Frame = [f32; buzztalk_core::FRAME_SAMPLES];

// ── Timing constants ─────────────────────────────────────────────────────────
//
// Every duration below is compared against a locally accumulated
// `Duration`, driven exclusively by [`Input::Tick`]. Nothing in this crate
// calls `Instant::now()`, so a test can drive a decade of wall-clock time in
// a microsecond and the outcome is exactly reproducible.

/// How long [`SessionState::Listening`] may run with no user speech before
/// the session ends outright.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How long [`SessionState::AwaitingAgent`] may run with no agent text
/// before the machine gives up on this turn and returns to
/// [`SessionState::Listening`].
pub const AGENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard cap on [`SessionState::UserSpeaking`]: past this, the machine forces
/// an endpoint rather than trusting the detector to ever fire one.
pub const MAX_UTTERANCE: Duration = Duration::from_secs(30);

/// How long [`SessionState::Interrupting`] waits for a transcript to prove a
/// barge-in was real before retracting it as spurious (a cough, a chair
/// creak, residual echo the AEC missed).
pub const BARGE_IN_RETRACTION_WINDOW: Duration = Duration::from_millis(700);

/// How long [`SessionState::Finalizing`] waits for the STT stream to
/// deliver a final transcript before giving up on this turn and falling
/// back to [`SessionState::Listening`].
///
/// Measured worst-case STT re-decode latency tops out around 700 ms; 5 s is
/// roughly a 7x margin over that, generous enough to absorb a slow backend
/// or a retry without the machine ever noticing, while still being far
/// short of [`IDLE_TIMEOUT`] -- a stuck finalize doesn't leave the user
/// staring at a frozen mic for anywhere near that long.
pub const FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// How much audio history [`crate::PreRollBuffer`] keeps.
///
/// Barge-in confirmation costs real time (the detector needs a run of
/// frames before it trusts itself), and those ~40 ms are exactly the start
/// of whatever the user is saying -- often the word that carries the
/// correction ("**No**, stop..."). 500 ms is generous headroom over that
/// confirmation cost.
pub const PREROLL_DURATION: Duration = Duration::from_millis(500);

/// [`PREROLL_DURATION`] expressed in frames at [`buzztalk_core::FRAME_MS`].
pub const PREROLL_FRAMES: usize =
    (PREROLL_DURATION.as_millis() as usize) / (buzztalk_core::FRAME_MS as usize);

// ── Turn identity ────────────────────────────────────────────────────────────

/// Identifies one conversational turn: one user utterance through to the
/// agent's spoken reply draining (or the turn being abandoned early).
///
/// Monotonically increasing and never reused. Every [`Input`] that reports
/// the result of something asynchronous (an STT partial or final, an agent
/// text chunk, a submit result, a drained buffer) carries the `TurnId` it
/// belongs to, and [`crate::SessionMachine::handle`] rejects it outright if
/// that turn is no longer current -- identity is checked before any state
/// logic runs. A caller learns the active `TurnId` from
/// [`crate::SessionMachine::current_turn`] right after a call that starts a
/// turn (e.g. [`Input::EndpointEvent`] carrying `SpeechStart`,
/// [`Input::PushToTalkPressed`]), or directly from an [`Output`] that hands
/// one out ([`Output::SubmitUtterance`], [`Output::ReplayPreRoll`],
/// [`Output::CancelSynthesis`], [`Output::ResumeSpeaking`]) -- and tags its
/// own async work with it so a result can be handed back unambiguously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnId(pub(crate) u64);

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "turn#{}", self.0)
    }
}

// ── Session state ────────────────────────────────────────────────────────────

/// The conversation's current phase.
///
/// Exactly one of these is active at a time; [`crate::SessionMachine::state`]
/// always reflects it, and [`Output::EmitState`] is produced every time it
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// No session running. Capture is stopped.
    Idle,
    /// Session running, mic open, waiting for the user to start talking.
    Listening,
    /// The user is talking; partial transcripts are expected.
    UserSpeaking,
    /// The endpoint fired; waiting for the STT stream to finalize.
    Finalizing,
    /// The final transcript was handed off; waiting on the network.
    Submitting,
    /// The turn was accepted; waiting for the agent's first reply text.
    AwaitingAgent,
    /// The agent's reply is being synthesized and/or played back.
    AgentSpeaking,
    /// A barge-in was confirmed by the detector but not yet proven real by
    /// a transcript; playback is paused and pending retraction.
    Interrupting,
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// Everything the outside world can hand the state machine.
///
/// The machine never polls anything -- every fact it knows about the world
/// arrives as one of these, including the passage of time ([`Input::Tick`]).
///
/// Every variant that reports the result of asynchronous work carries the
/// [`TurnId`] it belongs to. [`crate::SessionMachine::handle`] rejects any
/// such input whose turn is not [current](crate::SessionMachine::is_current)
/// before it even looks at the current [`SessionState`] -- a late result
/// for an abandoned turn (a stale STT finalize, a stale agent chunk) is
/// dropped unconditionally, never rendered.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Start a new session.
    SessionStart,
    /// End the session (graceful/remote-initiated).
    SessionEnd,
    /// A result from the endpointing (not barge-in) speech detector.
    EndpointEvent(DetectorEvent),
    /// The barge-in detector confirmed candidate speech during playback.
    BargeInConfirmed,
    /// An interim STT hypothesis for the in-progress utterance.
    PartialTranscript {
        /// The turn this hypothesis belongs to.
        turn: TurnId,
        /// The hypothesis text.
        text: String,
    },
    /// The STT stream finalized the utterance.
    FinalTranscript {
        /// The turn this transcript belongs to.
        turn: TurnId,
        /// The finalized text.
        text: String,
    },
    /// The submitted utterance was accepted by the agent backend.
    SubmitSucceeded {
        /// The turn that was submitted.
        turn: TurnId,
    },
    /// The submitted utterance failed to reach/be accepted by the backend.
    SubmitFailed {
        /// The turn that failed to submit.
        turn: TurnId,
        /// Why it failed.
        error: String,
    },
    /// A chunk of the agent's reply text arrived.
    AgentTextArrived {
        /// The turn this reply belongs to.
        turn: TurnId,
        /// The chunk of text.
        text: String,
    },
    /// The agent has no more text coming for this turn.
    AgentTurnComplete {
        /// The turn that just finished producing text.
        turn: TurnId,
    },
    /// The output audio buffer has fully drained.
    PlaybackDrained {
        /// The turn whose audio just finished playing.
        turn: TurnId,
    },
    /// Caller wants the session torn down now (e.g. a stop button).
    StopRequested,
    /// The push-to-talk control was pressed.
    PushToTalkPressed,
    /// The push-to-talk control was released.
    PushToTalkReleased,
    /// Time has advanced by this much since the last `Tick`.
    Tick(Duration),
}

// ── Outputs ──────────────────────────────────────────────────────────────────

/// Everything the state machine asks the caller to do.
///
/// The machine performs no side effects of its own -- it only describes
/// what should happen next. Order within one [`crate::SessionMachine::handle`]
/// call's returned `Vec` is significant and intentional.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Open the microphone / transport and start capturing.
    StartCapture,
    /// Close the microphone / transport.
    StopCapture,
    /// Render an interim transcript to the UI.
    ShowPartial(String),
    /// Send this finalized utterance to the agent backend.
    ///
    /// Carries the [`TurnId`] so the caller can tag the outbound request
    /// and hand it straight back in [`Input::SubmitSucceeded`] or
    /// [`Input::SubmitFailed`] when the round trip completes.
    SubmitUtterance {
        /// The turn this utterance belongs to.
        turn: TurnId,
        /// The finalized utterance text.
        text: String,
    },
    /// Stop playing the agent's audio immediately.
    CancelPlayback,
    /// Discard any buffered-but-unplayed output audio.
    FlushOutputBuffer,
    /// Stop generating any further audio for this turn.
    CancelSynthesis(TurnId),
    /// Replay the buffered pre-roll audio (the start of what the user said).
    ///
    /// Carries the [`TurnId`] just allocated for the barge-in candidate --
    /// the caller should tag whatever STT stream it starts over that
    /// replayed (and then live) audio with this turn, so a confirming
    /// [`Input::PartialTranscript`]/[`Input::FinalTranscript`] is accepted.
    ReplayPreRoll(TurnId),
    /// A barge-in turned out to be spurious; resume the agent's reply from
    /// the next un-synthesized chunk rather than discarding it.
    ///
    /// Carries the [`TurnId`] of the agent turn to resume.
    ResumeSpeaking(TurnId),
    /// The session's state just changed to this.
    EmitState(SessionState),
}
