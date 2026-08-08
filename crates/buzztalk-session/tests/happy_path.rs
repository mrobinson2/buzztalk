//! The full round trip with no interruptions: speak, endpoint, submit,
//! agent replies, agent speaks, drains, back to `Listening`.

mod common;

use buzztalk_core::DetectorEvent;
use buzztalk_session::{Input, Output, SessionMachine, SessionState};

#[test]
fn full_turn_returns_to_listening() {
    let mut m = SessionMachine::new();
    assert_eq!(m.state(), SessionState::Idle);

    let out = m.handle(Input::SessionStart);
    assert_eq!(
        out,
        vec![
            Output::StartCapture,
            Output::EmitState(SessionState::Listening)
        ]
    );

    let out = m.handle(Input::EndpointEvent(DetectorEvent::SpeechStart));
    assert_eq!(out, vec![Output::EmitState(SessionState::UserSpeaking)]);
    let turn = m.current_turn().expect("turn allocated on speech start");

    let out = m.handle(Input::PartialTranscript {
        turn,
        text: "hel".into(),
    });
    assert_eq!(out, vec![Output::ShowPartial("hel".into())]);

    let out = m.handle(Input::PartialTranscript {
        turn,
        text: "hello".into(),
    });
    assert_eq!(out, vec![Output::ShowPartial("hello".into())]);

    let out = m.handle(Input::EndpointEvent(DetectorEvent::SpeechEnd));
    assert_eq!(out, vec![Output::EmitState(SessionState::Finalizing)]);

    let out = m.handle(Input::FinalTranscript {
        turn,
        text: "hello there".into(),
    });
    assert_eq!(
        out,
        vec![
            Output::SubmitUtterance {
                turn,
                text: "hello there".into(),
            },
            Output::EmitState(SessionState::Submitting),
        ]
    );
    // Still the same turn throughout the user-speech half of the cycle.
    assert!(m.is_current(turn));

    let out = m.handle(Input::SubmitSucceeded { turn });
    assert_eq!(out, vec![Output::EmitState(SessionState::AwaitingAgent)]);

    let out = m.handle(Input::AgentTextArrived {
        turn,
        text: "Hi ".into(),
    });
    assert_eq!(out, vec![Output::EmitState(SessionState::AgentSpeaking)]);
    assert!(m.is_current(turn), "agent's reply belongs to the same turn");

    // A second chunk of the same reply: no state change, nothing to emit.
    let out = m.handle(Input::AgentTextArrived {
        turn,
        text: "there!".into(),
    });
    assert_eq!(out, Vec::<Output>::new());

    let out = m.handle(Input::AgentTurnComplete { turn });
    assert_eq!(out, Vec::<Output>::new(), "still draining playback");
    assert_eq!(m.state(), SessionState::AgentSpeaking);

    let out = m.handle(Input::PlaybackDrained { turn });
    assert_eq!(out, vec![Output::EmitState(SessionState::Listening)]);

    assert_eq!(m.state(), SessionState::Listening);
    assert_eq!(m.current_turn(), None, "turn is over");
    assert!(!m.is_current(turn));
}
