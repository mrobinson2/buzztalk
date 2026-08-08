//! The single most important correctness property in this crate: a result
//! for a turn that is no longer current must be dropped, never rendered.

mod common;

use buzztalk_session::{Input, Output, SessionState};
use common::{to_agent_speaking, to_finalizing};

#[test]
fn stale_final_transcript_from_a_cancelled_turn_is_ignored() {
    let (mut m, old_turn) = to_finalizing();
    assert_eq!(m.state(), SessionState::Finalizing);

    // Push-to-talk seizes the turn before the old turn's final transcript
    // ever arrives -- the old turn is abandoned.
    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(out, vec![Output::EmitState(SessionState::UserSpeaking)]);
    let new_turn = m.current_turn().expect("ptt started a new turn");
    assert_ne!(new_turn, old_turn);
    assert!(!m.is_current(old_turn));

    // The old turn's STT stream finally finalizes, late.
    let out = m.handle(Input::FinalTranscript("this arrived too late".into()));
    assert_eq!(
        out,
        Vec::<Output>::new(),
        "a final transcript is only accepted while Finalizing; we are not"
    );
    assert_eq!(
        m.state(),
        SessionState::UserSpeaking,
        "the stale transcript must not have moved the machine"
    );
    assert_eq!(m.current_turn(), Some(new_turn));
    assert!(!m.is_current(old_turn));
}

#[test]
fn stale_agent_text_from_a_cancelled_turn_is_ignored() {
    let (mut m, old_turn) = to_agent_speaking("what time is it", "It's currently");

    let _ = m.handle(Input::BargeInConfirmed);
    let out = m.handle(Input::PartialTranscript("Actually never mind".into()));
    assert_eq!(out[0], Output::CancelSynthesis(old_turn));
    let new_turn = m.current_turn().expect("barge-in started a new turn");
    assert_ne!(new_turn, old_turn);
    assert_eq!(m.state(), SessionState::UserSpeaking);

    // One more chunk from the old (now-cancelled) agent stream arrives late.
    let out = m.handle(Input::AgentTextArrived(" three fifteen.".into()));
    assert_eq!(
        out,
        Vec::<Output>::new(),
        "agent text is only accepted in AwaitingAgent/AgentSpeaking; we are in UserSpeaking"
    );
    assert_eq!(m.state(), SessionState::UserSpeaking);
    assert_eq!(m.current_turn(), Some(new_turn));
    assert!(!m.is_current(old_turn));
}
