//! Shared fixtures for driving a [`SessionMachine`] to a known point without
//! repeating the same bootstrap sequence in every test file.
//!
//! Not itself a test target: `tests/common/` is a subdirectory, so cargo
//! does not compile it as its own integration test binary. Each test file
//! pulls it in with `mod common;`.

#![allow(dead_code)]

use buzztalk_core::DetectorEvent;
use buzztalk_session::{Input, SessionMachine, TurnId};

/// A machine with `SessionStart` already applied.
pub fn started() -> SessionMachine {
    let mut m = SessionMachine::new();
    let _ = m.handle(Input::SessionStart);
    m
}

/// Drive a fresh session to `Finalizing`, having just endpointed a user
/// utterance. Returns the machine and the turn that utterance owns.
pub fn to_finalizing() -> (SessionMachine, TurnId) {
    let mut m = started();
    let _ = m.handle(Input::EndpointEvent(DetectorEvent::SpeechStart));
    let turn = m.current_turn().expect("turn active after SpeechStart");
    let _ = m.handle(Input::EndpointEvent(DetectorEvent::SpeechEnd));
    (m, turn)
}

/// Drive a fresh session all the way to `AwaitingAgent` with `text` as the
/// finalized (and successfully submitted) utterance.
pub fn to_awaiting_agent(text: &str) -> (SessionMachine, TurnId) {
    let (mut m, turn) = to_finalizing();
    let _ = m.handle(Input::FinalTranscript {
        turn,
        text: text.to_string(),
    });
    let _ = m.handle(Input::SubmitSucceeded { turn });
    (m, turn)
}

/// Drive a fresh session all the way to `AgentSpeaking`, having submitted
/// `text` and received `agent_text` as the agent's first reply chunk.
pub fn to_agent_speaking(text: &str, agent_text: &str) -> (SessionMachine, TurnId) {
    let (mut m, turn) = to_awaiting_agent(text);
    let _ = m.handle(Input::AgentTextArrived {
        turn,
        text: agent_text.to_string(),
    });
    (m, turn)
}
