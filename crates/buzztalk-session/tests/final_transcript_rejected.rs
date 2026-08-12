//! A final transcript a caller judges not worth acting on (e.g.
//! `buzztalk-pipeline`'s micro-utterance fragment filter) must return the
//! turn to `Listening` immediately, the same way the `FINALIZE_TIMEOUT`
//! fallback does -- not be silently dropped into a stuck `Finalizing`, and
//! not routed through `SubmitFailed`, which means something different (a
//! real backend failure worth surfacing for retry).

mod common;

use buzztalk_session::{Input, Output, SessionState};
use common::{started, to_finalizing};

#[test]
fn rejecting_a_final_transcript_returns_to_listening_and_frees_the_turn() {
    let (mut m, turn) = to_finalizing();

    let out = m.handle(Input::FinalTranscriptRejected { turn });

    assert_eq!(out, vec![Output::EmitState(SessionState::Listening)]);
    assert_eq!(m.state(), SessionState::Listening);
    assert!(
        !m.is_current(turn),
        "the rejected turn must no longer be current"
    );
}

#[test]
fn rejecting_a_final_transcript_does_not_touch_the_failed_utterance_record() {
    // Distinguishing this from `SubmitFailed` is the whole point: a
    // filtered-out fragment is not a failure worth surfacing for retry.
    let (mut m, turn) = to_finalizing();

    let _ = m.handle(Input::FinalTranscriptRejected { turn });

    assert_eq!(m.last_failed_utterance(), None);
    assert_eq!(m.last_submit_error(), None);
}

#[test]
fn a_stale_turn_cannot_be_rejected() {
    let (mut m, turn) = to_finalizing();
    // A second turn supersedes the first via PTT seizing control.
    let _ = m.handle(Input::PushToTalkPressed);
    let newer_turn = m.current_turn().expect("ptt starts a new turn");
    assert_ne!(turn, newer_turn);

    let out = m.handle(Input::FinalTranscriptRejected { turn });

    assert_eq!(
        out,
        Vec::<Output>::new(),
        "a stale turn's rejection is a no-op"
    );
    assert_eq!(
        m.current_turn(),
        Some(newer_turn),
        "the newer turn must be untouched"
    );
}

#[test]
fn rejection_outside_finalizing_is_a_no_op() {
    // Defense in depth, mirroring `on_final_transcript`'s own belt-and-braces
    // state check: nothing but `Finalizing` is ever waiting on a final
    // transcript to accept or reject. `m` is freshly started (`Listening`,
    // no current turn); `turn` is a real `TurnId` from an unrelated machine,
    // so it can never be `m`'s current turn either way.
    let (_other, turn) = to_finalizing();
    let mut m = started();
    let out = m.handle(Input::FinalTranscriptRejected { turn });
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::Listening);
}
