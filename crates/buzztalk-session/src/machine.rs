//! The conversation state machine.

use std::time::Duration;

use buzztalk_core::DetectorEvent;

use crate::preroll::PreRollBuffer;
use crate::types::{
    Frame, Input, Output, SessionState, TurnId, AGENT_RESPONSE_TIMEOUT, BARGE_IN_RETRACTION_WINDOW,
    FINALIZE_TIMEOUT, IDLE_TIMEOUT, MAX_UTTERANCE,
};

/// The conversation state machine.
///
/// Drives entirely off injected [`Input`]s and never performs a side effect
/// itself -- every [`handle`](SessionMachine::handle) call returns the
/// [`Output`]s the caller should act on, in the order they should happen.
///
/// This is deliberately transport-agnostic: nothing here reads a device,
/// calls an engine, or touches the clock. The same machine drives a desktop
/// mic/speaker session today and a telephony session later, fed by whatever
/// produces [`Input`]s for that transport.
#[derive(Debug, Clone)]
pub struct SessionMachine {
    state: SessionState,
    /// The turn presently accepted by the machine: the one a matching
    /// [`Input`] must name to be acted on.
    current_turn: Option<TurnId>,
    /// While [`SessionState::Interrupting`]: the agent turn that was
    /// speaking when the barge-in was confirmed, on hold pending proof it
    /// was genuine. `current_turn` holds the *candidate* turn during this
    /// window (see [`Self::on_barge_in_confirmed`]) so that a confirming
    /// transcript -- which necessarily arrives tagged with the candidate,
    /// not the agent turn -- passes the identity check unmodified.
    pending_cancel_turn: Option<TurnId>,
    turn_counter: u64,
    current_utterance: Option<String>,
    last_failed_utterance: Option<String>,
    last_submit_error: Option<String>,
    agent_turn_complete: bool,
    listening_elapsed: Duration,
    speaking_elapsed: Duration,
    finalizing_elapsed: Duration,
    awaiting_agent_elapsed: Duration,
    interrupting_elapsed: Duration,
    preroll: PreRollBuffer,
}

impl SessionMachine {
    /// A fresh machine, [`SessionState::Idle`], nothing buffered.
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            current_turn: None,
            pending_cancel_turn: None,
            turn_counter: 0,
            current_utterance: None,
            last_failed_utterance: None,
            last_submit_error: None,
            agent_turn_complete: false,
            listening_elapsed: Duration::ZERO,
            speaking_elapsed: Duration::ZERO,
            finalizing_elapsed: Duration::ZERO,
            awaiting_agent_elapsed: Duration::ZERO,
            interrupting_elapsed: Duration::ZERO,
            preroll: PreRollBuffer::new(),
        }
    }

    /// The current phase of the conversation.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The turn presently in flight, if any.
    ///
    /// Call this right after a call to [`Self::handle`] that starts a new
    /// turn (e.g. [`Input::EndpointEvent`] carrying `SpeechStart`, or
    /// [`Input::PushToTalkPressed`]) to learn the [`TurnId`] to tag that
    /// turn's STT results with -- those transitions don't hand the id out
    /// through an `Output` because nothing asynchronous has started yet.
    pub fn current_turn(&self) -> Option<TurnId> {
        self.current_turn
    }

    /// Whether `id` is still the active turn.
    ///
    /// [`Self::handle`] already enforces this for every turn-carrying
    /// [`Input`] -- rejecting a stale one before any state logic runs -- so
    /// correctness never depends on a caller checking this first. It's
    /// exposed as a cheap pre-filter: skip constructing/sending an `Input`
    /// at all for a result you already know is stale. Do **not** use it to
    /// decide whether to *abort* in-flight work (e.g. stop generating an
    /// agent reply) -- during [`SessionState::Interrupting`] the turn being
    /// held for possible resumption is briefly not current, precisely
    /// because it might resume. [`Output::CancelSynthesis`] is the only
    /// authoritative "stop for good" signal.
    pub fn is_current(&self, id: TurnId) -> bool {
        self.current_turn == Some(id)
    }

    /// The text of the most recent utterance whose submission failed.
    ///
    /// [`Output`] has no variant for "show this error" -- the machine keeps
    /// the words instead of discarding them, so the caller can retrieve
    /// them (e.g. to prefill a retry) after handling
    /// [`Input::SubmitFailed`].
    pub fn last_failed_utterance(&self) -> Option<&str> {
        self.last_failed_utterance.as_deref()
    }

    /// The reason string from the most recent [`Input::SubmitFailed`].
    pub fn last_submit_error(&self) -> Option<&str> {
        self.last_submit_error.as_deref()
    }

    /// Feed one frame of audio into the pre-roll buffer.
    ///
    /// A no-op while [`SessionState::Idle`] -- the buffer only fills while a
    /// session is actually active, and is cleared when the session ends so
    /// stale audio from a previous session can never be replayed into a new
    /// one.
    pub fn push_frame(&mut self, frame: Frame) {
        if self.state != SessionState::Idle {
            self.preroll.push(frame);
        }
    }

    /// A snapshot of the buffered pre-roll audio, oldest first -- the
    /// replay order -- for the caller to act on after receiving
    /// [`Output::ReplayPreRoll`].
    pub fn preroll_frames(&self) -> Vec<Frame> {
        self.preroll.frames_oldest_first()
    }

    /// Drive the machine with one input, returning the outputs to act on in
    /// order.
    pub fn handle(&mut self, input: Input) -> Vec<Output> {
        let prev_state = self.state;
        let mut out = match input {
            Input::SessionStart => self.on_session_start(),
            Input::SessionEnd | Input::StopRequested => self.on_session_end(),
            Input::EndpointEvent(ev) => self.on_endpoint_event(ev),
            Input::BargeInConfirmed => self.on_barge_in_confirmed(),
            Input::PartialTranscript { turn, text } => self.on_partial_transcript(turn, text),
            Input::FinalTranscript { turn, text } => self.on_final_transcript(turn, text),
            Input::SubmitSucceeded { turn } => self.on_submit_succeeded(turn),
            Input::SubmitFailed { turn, error } => self.on_submit_failed(turn, error),
            Input::AgentTextArrived { turn, text } => self.on_agent_text_arrived(turn, text),
            Input::AgentTurnComplete { turn } => self.on_agent_turn_complete(turn),
            Input::PlaybackDrained { turn } => self.on_playback_drained(turn),
            Input::PushToTalkPressed => self.on_ptt_pressed(),
            Input::PushToTalkReleased => self.on_ptt_released(),
            Input::Tick(dt) => self.on_tick(dt),
        };
        if self.state != prev_state {
            out.push(Output::EmitState(self.state));
        }
        out
    }

    // ── Turn lifecycle ───────────────────────────────────────────────────

    /// Allocate a new turn and make it current. Called exactly at the
    /// moment a fresh user utterance begins.
    fn begin_user_turn(&mut self) -> TurnId {
        self.turn_counter += 1;
        let id = TurnId(self.turn_counter);
        self.current_turn = Some(id);
        self.speaking_elapsed = Duration::ZERO;
        self.current_utterance = None;
        id
    }

    /// A turn completed successfully end-to-end: no more active turn, back
    /// to listening for the next one.
    fn end_turn_return_to_listening(&mut self) {
        self.current_turn = None;
        self.agent_turn_complete = false;
        self.state = SessionState::Listening;
        self.listening_elapsed = Duration::ZERO;
    }

    fn reset_all_timers(&mut self) {
        self.listening_elapsed = Duration::ZERO;
        self.speaking_elapsed = Duration::ZERO;
        self.finalizing_elapsed = Duration::ZERO;
        self.awaiting_agent_elapsed = Duration::ZERO;
        self.interrupting_elapsed = Duration::ZERO;
    }

    // ── Input handlers ───────────────────────────────────────────────────

    fn on_session_start(&mut self) -> Vec<Output> {
        if self.state != SessionState::Idle {
            return vec![];
        }
        self.reset_all_timers();
        self.preroll.clear();
        self.current_turn = None;
        self.pending_cancel_turn = None;
        self.current_utterance = None;
        self.last_failed_utterance = None;
        self.last_submit_error = None;
        self.agent_turn_complete = false;
        self.state = SessionState::Listening;
        vec![Output::StartCapture]
    }

    fn on_session_end(&mut self) -> Vec<Output> {
        if self.state == SessionState::Idle {
            return vec![];
        }
        let mut out = Vec::new();
        if matches!(
            self.state,
            SessionState::AgentSpeaking | SessionState::Interrupting
        ) {
            out.push(Output::CancelPlayback);
            out.push(Output::FlushOutputBuffer);
            // While Interrupting, `current_turn` holds the barge-in
            // candidate, not the agent turn that's actually mid-synthesis
            // -- that one is parked in `pending_cancel_turn`.
            let turn_to_cancel = if self.state == SessionState::Interrupting {
                self.pending_cancel_turn
            } else {
                self.current_turn
            };
            if let Some(turn) = turn_to_cancel {
                out.push(Output::CancelSynthesis(turn));
            }
        }
        out.push(Output::StopCapture);
        self.state = SessionState::Idle;
        self.current_turn = None;
        self.pending_cancel_turn = None;
        self.agent_turn_complete = false;
        self.preroll.clear();
        self.reset_all_timers();
        out
    }

    fn on_endpoint_event(&mut self, ev: DetectorEvent) -> Vec<Output> {
        match (self.state, ev) {
            (SessionState::Listening, DetectorEvent::SpeechStart) => {
                self.begin_user_turn();
                self.state = SessionState::UserSpeaking;
                vec![]
            }
            (SessionState::UserSpeaking, DetectorEvent::SpeechEnd) => {
                self.state = SessionState::Finalizing;
                self.finalizing_elapsed = Duration::ZERO;
                vec![]
            }
            // SpeechContinue, an Idle event, or any combination outside the
            // two above (e.g. a stray endpoint event while the agent is
            // speaking) carries no actionable information here.
            _ => vec![],
        }
    }

    fn on_barge_in_confirmed(&mut self) -> Vec<Output> {
        // Only actionable while the agent is actually speaking. In
        // particular, if the agent's turn just finished (e.g. this and a
        // PlaybackDrained landed on the same tick and PlaybackDrained was
        // processed first), there is nothing left to interrupt -- ignore.
        if self.state != SessionState::AgentSpeaking {
            return vec![];
        }
        // Allocate the barge-in candidate's identity *now*, not when it's
        // later proven genuine: the caller needs it immediately to tag the
        // STT stream it starts over the replayed pre-roll audio. The agent
        // turn being interrupted is parked, not cancelled yet -- it may
        // still turn out to be a false alarm.
        self.pending_cancel_turn = self.current_turn;
        let candidate = self.begin_user_turn();
        self.interrupting_elapsed = Duration::ZERO;
        self.state = SessionState::Interrupting;
        vec![
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::ReplayPreRoll(candidate),
        ]
    }

    fn on_partial_transcript(&mut self, turn: TurnId, text: String) -> Vec<Output> {
        // Identity first, unconditionally, before any state logic: a
        // result for a turn that isn't current is dropped outright.
        if !self.is_current(turn) {
            return vec![];
        }
        match self.state {
            SessionState::UserSpeaking => vec![Output::ShowPartial(text)],
            SessionState::Interrupting => {
                self.confirm_genuine_barge_in(vec![Output::ShowPartial(text)])
            }
            // Not expecting a partial: Listening (no utterance yet),
            // Finalizing (already past partials, waiting on the final),
            // Submitting/AwaitingAgent/AgentSpeaking/Idle (no user turn
            // active). Belt-and-braces state check behind the identity
            // check above.
            _ => vec![],
        }
    }

    fn on_final_transcript(&mut self, turn: TurnId, text: String) -> Vec<Output> {
        if !self.is_current(turn) {
            return vec![];
        }
        match self.state {
            SessionState::Finalizing => {
                self.current_utterance = Some(text.clone());
                self.state = SessionState::Submitting;
                vec![Output::SubmitUtterance { turn, text }]
            }
            // A very fast interruption: the confirming transcript is
            // already the complete utterance.
            SessionState::Interrupting => {
                let mut out = self.confirm_genuine_barge_in(vec![]);
                self.current_utterance = Some(text.clone());
                self.state = SessionState::Submitting;
                out.push(Output::SubmitUtterance { turn, text });
                out
            }
            // Only Finalizing (or a confirming Interrupting) is actually
            // waiting on a final transcript. Anything else means this text
            // belongs to a turn that is, despite matching identity here in
            // a way that shouldn't be reachable, not in a state expecting
            // it -- drop it. (Kept as defense in depth; the identity check
            // above should already have caught every real-world case this
            // would guard against.)
            _ => vec![],
        }
    }

    /// Shared confirmation path for a genuine (non-spurious) barge-in: the
    /// parked agent turn is cancelled for real, and the machine moves into
    /// `UserSpeaking` for the (already-current) candidate turn.
    ///
    /// `extra` supplies whatever outputs the specific confirming input
    /// (partial or final transcript) implies; they're appended after the
    /// cancellation.
    fn confirm_genuine_barge_in(&mut self, extra: Vec<Output>) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(old_turn) = self.pending_cancel_turn.take() {
            out.push(Output::CancelSynthesis(old_turn));
        }
        self.speaking_elapsed = Duration::ZERO;
        self.state = SessionState::UserSpeaking;
        out.extend(extra);
        out
    }

    fn on_submit_succeeded(&mut self, turn: TurnId) -> Vec<Output> {
        if !self.is_current(turn) {
            return vec![];
        }
        if self.state != SessionState::Submitting {
            return vec![];
        }
        self.current_utterance = None;
        self.awaiting_agent_elapsed = Duration::ZERO;
        self.state = SessionState::AwaitingAgent;
        vec![]
    }

    fn on_submit_failed(&mut self, turn: TurnId, error: String) -> Vec<Output> {
        if !self.is_current(turn) {
            return vec![];
        }
        if self.state != SessionState::Submitting {
            return vec![];
        }
        // Keep the words: don't silently drop what the user said just
        // because the network call failed.
        self.last_failed_utterance = self.current_utterance.take();
        self.last_submit_error = Some(error);
        self.current_turn = None;
        self.state = SessionState::Listening;
        self.listening_elapsed = Duration::ZERO;
        vec![]
    }

    fn on_agent_text_arrived(&mut self, turn: TurnId, _text: String) -> Vec<Output> {
        // The text itself is not stored or replayed through an Output --
        // the caller already has it (it's the one calling `handle` with
        // it) and drives its own TTS/UI directly from the same event. This
        // machine only tracks the control-flow consequence: whether the
        // agent has started speaking for this turn.
        if !self.is_current(turn) {
            return vec![];
        }
        match self.state {
            SessionState::AwaitingAgent => {
                self.awaiting_agent_elapsed = Duration::ZERO;
                self.agent_turn_complete = false;
                self.state = SessionState::AgentSpeaking;
                vec![]
            }
            SessionState::AgentSpeaking => vec![],
            // Not expecting agent text: this chunk belongs to a turn this
            // machine has already abandoned. Belt-and-braces behind the
            // identity check above.
            _ => vec![],
        }
    }

    fn on_agent_turn_complete(&mut self, turn: TurnId) -> Vec<Output> {
        if !self.is_current(turn) {
            return vec![];
        }
        match self.state {
            // The agent had nothing to say at all this turn -- nothing to
            // play, so the turn is already over.
            SessionState::AwaitingAgent => {
                self.end_turn_return_to_listening();
                vec![]
            }
            SessionState::AgentSpeaking => {
                self.agent_turn_complete = true;
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_playback_drained(&mut self, turn: TurnId) -> Vec<Output> {
        if !self.is_current(turn) {
            return vec![];
        }
        if self.state != SessionState::AgentSpeaking {
            return vec![];
        }
        if self.agent_turn_complete {
            self.end_turn_return_to_listening();
        }
        // Otherwise: the output buffer merely ran dry ahead of synthesis
        // (a underrun) -- stay in AgentSpeaking and wait for more text or
        // for AgentTurnComplete.
        vec![]
    }

    fn on_ptt_pressed(&mut self) -> Vec<Output> {
        match self.state {
            // No session, or already the user's turn: nothing to override.
            SessionState::Idle | SessionState::UserSpeaking => vec![],
            SessionState::AgentSpeaking => {
                let mut out = vec![Output::CancelPlayback, Output::FlushOutputBuffer];
                if let Some(turn) = self.current_turn {
                    out.push(Output::CancelSynthesis(turn));
                }
                self.begin_user_turn();
                self.state = SessionState::UserSpeaking;
                out
            }
            SessionState::Interrupting => {
                // Playback was already cancelled/flushed on the way into
                // Interrupting; only the parked agent turn still needs
                // cancelling. The candidate turn (already `current_turn`)
                // becomes the user's turn for real -- PTT is itself proof
                // this is genuine, no transcript needed.
                let mut out = Vec::new();
                if let Some(old_turn) = self.pending_cancel_turn.take() {
                    out.push(Output::CancelSynthesis(old_turn));
                }
                self.speaking_elapsed = Duration::ZERO;
                self.current_utterance = None;
                self.state = SessionState::UserSpeaking;
                out
            }
            SessionState::Listening
            | SessionState::Finalizing
            | SessionState::Submitting
            | SessionState::AwaitingAgent => {
                // Push-to-talk overrides whatever gating logic would
                // otherwise apply (waiting on an endpoint, a submit
                // response, or the agent) and unconditionally seizes the
                // turn for the user.
                self.begin_user_turn();
                self.state = SessionState::UserSpeaking;
                vec![]
            }
        }
    }

    fn on_ptt_released(&mut self) -> Vec<Output> {
        if self.state != SessionState::UserSpeaking {
            return vec![];
        }
        self.state = SessionState::Finalizing;
        self.finalizing_elapsed = Duration::ZERO;
        vec![]
    }

    fn on_tick(&mut self, dt: Duration) -> Vec<Output> {
        match self.state {
            SessionState::Listening => {
                self.listening_elapsed += dt;
                if self.listening_elapsed >= IDLE_TIMEOUT {
                    return self.on_session_end();
                }
            }
            SessionState::UserSpeaking => {
                self.speaking_elapsed += dt;
                if self.speaking_elapsed >= MAX_UTTERANCE {
                    // Don't trust the detector to ever endpoint; force it.
                    self.state = SessionState::Finalizing;
                    self.speaking_elapsed = Duration::ZERO;
                    self.finalizing_elapsed = Duration::ZERO;
                }
            }
            SessionState::Finalizing => {
                self.finalizing_elapsed += dt;
                if self.finalizing_elapsed >= FINALIZE_TIMEOUT {
                    // The STT stream never finalized -- don't hang here
                    // forever. Drop the turn and give the mic back.
                    self.finalizing_elapsed = Duration::ZERO;
                    self.current_turn = None;
                    self.state = SessionState::Listening;
                    self.listening_elapsed = Duration::ZERO;
                }
            }
            SessionState::AwaitingAgent => {
                self.awaiting_agent_elapsed += dt;
                if self.awaiting_agent_elapsed >= AGENT_RESPONSE_TIMEOUT {
                    self.current_turn = None;
                    self.awaiting_agent_elapsed = Duration::ZERO;
                    self.state = SessionState::Listening;
                    self.listening_elapsed = Duration::ZERO;
                }
            }
            SessionState::Interrupting => {
                self.interrupting_elapsed += dt;
                if self.interrupting_elapsed >= BARGE_IN_RETRACTION_WINDOW {
                    self.interrupting_elapsed = Duration::ZERO;
                    // Restore the parked agent turn as current -- it never
                    // stopped being the one and only active turn, it was
                    // just shadowed by the (now-discarded) candidate.
                    let resumed = self.pending_cancel_turn.take();
                    self.current_turn = resumed;
                    self.state = SessionState::AgentSpeaking;
                    if let Some(turn) = resumed {
                        return vec![Output::ResumeSpeaking(turn)];
                    }
                }
            }
            // No independent timer of their own: Idle and AgentSpeaking are
            // unbounded by design; Submitting is bounded by whatever the
            // caller's network stack does, not by this machine.
            SessionState::Idle | SessionState::Submitting | SessionState::AgentSpeaking => {}
        }
        vec![]
    }
}

impl Default for SessionMachine {
    fn default() -> Self {
        Self::new()
    }
}
