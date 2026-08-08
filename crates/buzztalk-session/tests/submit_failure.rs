//! A failed submission must not silently discard what the user said.

mod common;

use buzztalk_core::DetectorEvent;
use buzztalk_session::{Input, Output, SessionState};
use common::{started, to_finalizing};

#[test]
fn submit_failure_keeps_the_text_and_returns_to_listening() {
    let (mut m, turn) = to_finalizing();

    let out = m.handle(Input::FinalTranscript {
        turn,
        text: "turn off the porch light".into(),
    });
    assert_eq!(
        out,
        vec![
            Output::SubmitUtterance {
                turn,
                text: "turn off the porch light".into(),
            },
            Output::EmitState(SessionState::Submitting),
        ]
    );

    let out = m.handle(Input::SubmitFailed {
        turn,
        error: "network unreachable".into(),
    });
    assert_eq!(out, vec![Output::EmitState(SessionState::Listening)]);

    assert_eq!(m.state(), SessionState::Listening);
    assert_eq!(
        m.last_failed_utterance(),
        Some("turn off the porch light"),
        "the words must survive the failure"
    );
    assert_eq!(m.last_submit_error(), Some("network unreachable"));
    assert!(!m.is_current(turn), "the failed turn is over");
}

#[test]
fn submit_result_arriving_outside_submitting_is_ignored_even_for_the_current_turn() {
    let mut m = started();
    let _ = m.handle(Input::EndpointEvent(DetectorEvent::SpeechStart));
    let turn = m.current_turn().expect("turn active");
    assert_eq!(m.state(), SessionState::UserSpeaking, "not yet Submitting");

    // Identity matches (it's the current turn), but the state doesn't --
    // the belt-and-braces state check still applies behind identity.
    let out = m.handle(Input::SubmitFailed {
        turn,
        error: "stray".into(),
    });
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::UserSpeaking);
    assert_eq!(m.last_failed_utterance(), None);
}

#[test]
fn submit_result_for_a_turn_that_is_not_current_is_ignored() {
    let (mut m, old_turn) = to_finalizing();

    // Push-to-talk abandons the old turn before it ever reaches Submitting.
    let _ = m.handle(Input::PushToTalkPressed);
    let new_turn = m.current_turn().expect("ptt started a new turn");
    assert_ne!(new_turn, old_turn);

    let out = m.handle(Input::SubmitFailed {
        turn: old_turn,
        error: "stray, for an abandoned turn".into(),
    });
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.last_failed_utterance(), None);
}
