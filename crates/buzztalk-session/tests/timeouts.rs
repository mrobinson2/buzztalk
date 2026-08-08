//! All timeouts are driven purely by `Input::Tick`, never a wall clock.

mod common;

use std::time::Duration;

use buzztalk_core::DetectorEvent;
use buzztalk_session::{Input, Output, SessionMachine, SessionState};
use common::{started, to_awaiting_agent};

#[test]
fn idle_timeout_ends_the_session() {
    let mut m = started();
    assert_eq!(m.state(), SessionState::Listening);

    let out = m.handle(Input::Tick(Duration::from_secs(89)));
    assert_eq!(out, Vec::<Output>::new(), "not yet at the 90s threshold");
    assert_eq!(m.state(), SessionState::Listening);

    let out = m.handle(Input::Tick(Duration::from_secs(1)));
    assert_eq!(
        out,
        vec![Output::StopCapture, Output::EmitState(SessionState::Idle)],
        "crossing 90s idle ends the whole session, not just the turn"
    );
    assert_eq!(m.state(), SessionState::Idle);
}

#[test]
fn agent_response_timeout_returns_to_listening_not_ending_the_session() {
    let (mut m, turn) = to_awaiting_agent("are you there");
    assert_eq!(m.state(), SessionState::AwaitingAgent);

    let out = m.handle(Input::Tick(Duration::from_secs(59)));
    assert_eq!(out, Vec::<Output>::new());

    let out = m.handle(Input::Tick(Duration::from_secs(1)));
    assert_eq!(
        out,
        vec![Output::EmitState(SessionState::Listening)],
        "agent timeout returns to Listening, unlike idle timeout ending the session"
    );
    assert_eq!(m.state(), SessionState::Listening);
    assert!(!m.is_current(turn));
}

#[test]
fn max_utterance_forces_an_endpoint_after_30s() {
    let mut m = started();
    let _ = m.handle(Input::EndpointEvent(DetectorEvent::SpeechStart));
    assert_eq!(m.state(), SessionState::UserSpeaking);

    let out = m.handle(Input::Tick(Duration::from_secs(29)));
    assert_eq!(out, Vec::<Output>::new());

    let out = m.handle(Input::Tick(Duration::from_secs(1)));
    assert_eq!(out, vec![Output::EmitState(SessionState::Finalizing)]);
    assert_eq!(m.state(), SessionState::Finalizing);
}

#[test]
fn finalize_timeout_falls_back_to_listening_after_5s() {
    let (mut m, turn) = common::to_finalizing();
    assert_eq!(m.state(), SessionState::Finalizing);

    let out = m.handle(Input::Tick(Duration::from_secs(4)));
    assert_eq!(out, Vec::<Output>::new(), "not yet at the 5s threshold");
    assert_eq!(m.state(), SessionState::Finalizing);

    let out = m.handle(Input::Tick(Duration::from_secs(1)));
    assert_eq!(
        out,
        vec![Output::EmitState(SessionState::Listening)],
        "a stuck STT finalize falls back to Listening rather than hanging forever"
    );
    assert_eq!(m.state(), SessionState::Listening);
    assert!(!m.is_current(turn));

    // Sanity: the abandoned turn's final transcript, if it ever does show
    // up, is now stale and dropped like any other late result.
    let out = m.handle(Input::FinalTranscript {
        turn,
        text: "far too late".into(),
    });
    assert_eq!(out, Vec::<Output>::new());
}

#[test]
fn ticks_in_states_with_no_active_timer_are_inert() {
    let mut m = SessionMachine::new();
    assert_eq!(
        m.handle(Input::Tick(Duration::from_secs(1_000))),
        Vec::<Output>::new()
    );
    assert_eq!(m.state(), SessionState::Idle);
}
