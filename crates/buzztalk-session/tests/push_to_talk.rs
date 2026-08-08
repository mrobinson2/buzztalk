//! Push-to-talk is a hard override: it ignores whatever gating logic would
//! normally apply and, when the agent is talking, cancels playback
//! unconditionally.

mod common;

use buzztalk_session::{Input, Output, SessionMachine, SessionState};
use common::{started, to_agent_speaking};

#[test]
fn ptt_while_agent_speaking_cancels_playback_and_synthesis_unconditionally() {
    let (mut m, old_turn) = to_agent_speaking("what's next", "Let's see, next up is");

    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(
        out,
        vec![
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::CancelSynthesis(old_turn),
            Output::EmitState(SessionState::UserSpeaking),
        ]
    );
    assert_eq!(m.state(), SessionState::UserSpeaking);
    assert!(!m.is_current(old_turn));
}

#[test]
fn ptt_press_then_release_finalizes_like_a_natural_endpoint() {
    let mut m = started();
    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(out, vec![Output::EmitState(SessionState::UserSpeaking)]);

    let out = m.handle(Input::PushToTalkReleased);
    assert_eq!(out, vec![Output::EmitState(SessionState::Finalizing)]);
}

#[test]
fn ptt_pressed_while_idle_is_a_no_op() {
    let mut m = SessionMachine::new();
    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::Idle);
}

#[test]
fn ptt_pressed_while_already_speaking_is_a_no_op() {
    let mut m = started();
    let _ = m.handle(Input::PushToTalkPressed);
    let turn = m.current_turn();

    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(
        m.current_turn(),
        turn,
        "pressing again must not churn the turn"
    );
}

#[test]
fn ptt_pressed_while_interrupting_confirms_it_and_cancels_the_parked_agent_turn() {
    let (mut m, old_turn) = to_agent_speaking("what's next", "Let's see, next up is");

    let _ = m.handle(Input::BargeInConfirmed);
    let candidate = m.current_turn().expect("candidate turn while Interrupting");
    assert_eq!(m.state(), SessionState::Interrupting);

    // Playback was already cancelled/flushed on the way into Interrupting;
    // PTT only needs to finish cancelling the parked agent turn for real
    // and confirm the candidate as a genuine user turn -- no transcript
    // needed, the button press is itself the proof.
    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(
        out,
        vec![
            Output::CancelSynthesis(old_turn),
            Output::EmitState(SessionState::UserSpeaking),
        ]
    );
    assert_eq!(m.state(), SessionState::UserSpeaking);
    assert_eq!(m.current_turn(), Some(candidate));
    assert!(!m.is_current(old_turn));
}

#[test]
fn ptt_released_without_a_prior_press_is_ignored() {
    let mut m = started();
    let out = m.handle(Input::PushToTalkReleased);
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::Listening);
}
