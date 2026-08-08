//! The conversation state machine.

use std::time::Duration;

use buzztalk_core::DetectorEvent;

use crate::preroll::PreRollBuffer;
use crate::types::{
    Frame, Input, Output, SessionState, TurnId, AGENT_RESPONSE_TIMEOUT, BARGE_IN_RETRACTION_WINDOW,
    IDLE_TIMEOUT, MAX_UTTERANCE,
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
    current_turn: Option<TurnId>,
    turn_counter: u64,
    current_utterance: Option<String>,
    last_failed_utterance: Option<String>,
    last_submit_error: Option<String>,
    agent_turn_complete: bool,
    listening_elapsed: Duration,
    speaking_elapsed: Duration,
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
            turn_counter: 0,
            current_utterance: None,
            last_failed_utterance: None,
            last_submit_error: None,
            agent_turn_complete: false,
            listening_elapsed: Duration::ZERO,
            speaking_elapsed: Duration::ZERO,
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
    pub fn current_turn(&self) -> Option<TurnId> {
        self.current_turn
    }

    /// Whether `id` is still the active turn.
    ///
    /// Callers holding a [`TurnId`] captured earlier (e.g. alongside an
    /// outbound agent request) should check this before acting on a result
    /// that arrives asynchronously. The machine itself applies the same
    /// rule internally: [`Input::FinalTranscript`] and
    /// [`Input::AgentTextArrived`] are only accepted in the states that are
    /// actually waiting on them, so a result for an abandoned turn is
    /// dropped rather than rendered even if the caller forwards it anyway.
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
            Input::PartialTranscript(text) => self.on_partial_transcript(text),
            Input::FinalTranscript(text) => self.on_final_transcript(text),
            Input::SubmitSucceeded => self.on_submit_succeeded(),
            Input::SubmitFailed(reason) => self.on_submit_failed(reason),
            Input::AgentTextArrived(text) => self.on_agent_text_arrived(text),
            Input::AgentTurnComplete => self.on_agent_turn_complete(),
            Input::PlaybackDrained => self.on_playback_drained(),
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
            if let Some(turn) = self.current_turn {
                out.push(Output::CancelSynthesis(turn));
            }
        }
        out.push(Output::StopCapture);
        self.state = SessionState::Idle;
        self.current_turn = None;
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
        self.interrupting_elapsed = Duration::ZERO;
        self.state = SessionState::Interrupting;
        vec![
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::ReplayPreRoll,
        ]
    }

    fn on_partial_transcript(&mut self, text: String) -> Vec<Output> {
        match self.state {
            SessionState::UserSpeaking => vec![Output::ShowPartial(text)],
            SessionState::Interrupting => {
                self.confirm_genuine_barge_in(move |_| vec![Output::ShowPartial(text)])
            }
            // Not expecting a partial: Listening (no utterance yet),
            // Finalizing (already past partials, waiting on the final),
            // Submitting/AwaitingAgent/AgentSpeaking/Idle (no user turn
            // active). A partial here belongs to an utterance this machine
            // has already moved on from -- drop it.
            _ => vec![],
        }
    }

    fn on_final_transcript(&mut self, text: String) -> Vec<Output> {
        match self.state {
            SessionState::Finalizing => {
                self.current_utterance = Some(text.clone());
                self.state = SessionState::Submitting;
                vec![Output::SubmitUtterance(text)]
            }
            // A very fast interruption: the confirming transcript is
            // already the complete utterance.
            SessionState::Interrupting => self.confirm_genuine_barge_in(move |m| {
                m.current_utterance = Some(text.clone());
                m.state = SessionState::Submitting;
                vec![Output::SubmitUtterance(text)]
            }),
            // Only Finalizing is actually waiting on a final transcript.
            // Anything else means this text belongs to a turn that is no
            // longer current -- drop it silently, per the crate's central
            // correctness rule: never render a late result for an
            // abandoned turn.
            _ => vec![],
        }
    }

    /// Shared confirmation path for a genuine (non-spurious) barge-in: the
    /// old turn's synthesis is cancelled for real, a fresh turn begins, and
    /// then `then` supplies whatever outputs the specific confirming input
    /// (partial or final transcript) implies.
    fn confirm_genuine_barge_in(
        &mut self,
        then: impl FnOnce(&mut Self) -> Vec<Output>,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(old_turn) = self.current_turn {
            out.push(Output::CancelSynthesis(old_turn));
        }
        self.begin_user_turn();
        self.state = SessionState::UserSpeaking;
        out.extend(then(self));
        out
    }

    fn on_submit_succeeded(&mut self) -> Vec<Output> {
        if self.state != SessionState::Submitting {
            return vec![];
        }
        self.current_utterance = None;
        self.awaiting_agent_elapsed = Duration::ZERO;
        self.state = SessionState::AwaitingAgent;
        vec![]
    }

    fn on_submit_failed(&mut self, reason: String) -> Vec<Output> {
        if self.state != SessionState::Submitting {
            return vec![];
        }
        // Keep the words: don't silently drop what the user said just
        // because the network call failed.
        self.last_failed_utterance = self.current_utterance.take();
        self.last_submit_error = Some(reason);
        self.current_turn = None;
        self.state = SessionState::Listening;
        self.listening_elapsed = Duration::ZERO;
        vec![]
    }

    fn on_agent_text_arrived(&mut self, _text: String) -> Vec<Output> {
        // The text itself is not stored or replayed through an Output --
        // the caller already has it (it's the one calling `handle` with
        // it) and drives its own TTS/UI directly from the same event. This
        // machine only tracks the control-flow consequence: whether the
        // agent has started speaking for this turn.
        match self.state {
            SessionState::AwaitingAgent => {
                self.awaiting_agent_elapsed = Duration::ZERO;
                self.agent_turn_complete = false;
                self.state = SessionState::AgentSpeaking;
                vec![]
            }
            SessionState::AgentSpeaking => vec![],
            // Not expecting agent text: this chunk belongs to a turn this
            // machine has already abandoned (e.g. a confirmed barge-in
            // moved it back to UserSpeaking). Drop it.
            _ => vec![],
        }
    }

    fn on_agent_turn_complete(&mut self) -> Vec<Output> {
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

    fn on_playback_drained(&mut self) -> Vec<Output> {
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
            SessionState::AgentSpeaking | SessionState::Interrupting => {
                let mut out = vec![Output::CancelPlayback, Output::FlushOutputBuffer];
                if let Some(turn) = self.current_turn {
                    out.push(Output::CancelSynthesis(turn));
                }
                self.begin_user_turn();
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
                    self.state = SessionState::AgentSpeaking;
                    return vec![Output::ResumeSpeaking];
                }
            }
            // No independent timer of their own: Idle and AgentSpeaking are
            // unbounded by design; Finalizing and Submitting are bounded by
            // whatever the caller's STT/network stack does, not by this
            // machine (see the crate-level docs for why that's a
            // deliberate gap rather than an invented number).
            SessionState::Idle
            | SessionState::Finalizing
            | SessionState::Submitting
            | SessionState::AgentSpeaking => {}
        }
        vec![]
    }
}

impl Default for SessionMachine {
    fn default() -> Self {
        Self::new()
    }
}
