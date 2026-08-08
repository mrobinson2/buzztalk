//! A failed submission must not silently discard what the user said.

mod common;

use buzztalk_session::{Input, Output, SessionState};
use common::{started, to_finalizing};

#[test]
fn submit_failure_keeps_the_text_and_returns_to_listening() {
    let (mut m, turn) = to_finalizing();

    let out = m.handle(Input::FinalTranscript("turn off the porch light".into()));
    assert_eq!(
        out,
        vec![
            Output::SubmitUtterance("turn off the porch light".into()),
            Output::EmitState(SessionState::Submitting),
        ]
    );

    let out = m.handle(Input::SubmitFailed("network unreachable".into()));
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
fn submit_result_arriving_outside_submitting_is_ignored() {
    let mut m = started();
    let out = m.handle(Input::SubmitFailed("stray".into()));
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::Listening);
    assert_eq!(m.last_failed_utterance(), None);
}
