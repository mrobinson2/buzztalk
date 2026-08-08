//! Confirmed barge-in mid-playback, spurious barge-in retraction, and the
//! race between the agent finishing and a barge-in landing on the same
//! tick.

mod common;

use std::time::Duration;

use buzztalk_session::{Input, Output, SessionState};
use common::to_agent_speaking;

#[test]
fn confirmed_barge_in_cancels_playback_flushes_and_replays_preroll_then_user_speaking() {
    let (mut m, old_turn) = to_agent_speaking("play some music", "Sure, starting playback now");

    // Frames captured while the agent was talking -- this is what gets
    // replayed so the user's interrupting word isn't lost to the ~40 ms
    // barge-in confirmation cost.
    m.push_frame([0.1; buzztalk_core::FRAME_SAMPLES]);
    m.push_frame([0.2; buzztalk_core::FRAME_SAMPLES]);
    let expected_preroll = m.preroll_frames();
    assert_eq!(expected_preroll.len(), 2);

    let out = m.handle(Input::BargeInConfirmed);
    assert_eq!(
        out,
        vec![
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::ReplayPreRoll,
            Output::EmitState(SessionState::Interrupting),
        ],
        "cancel, flush, and replay -- in that order -- before announcing the new state"
    );
    assert_eq!(m.state(), SessionState::Interrupting);
    // Not yet cancelled for real: still uncertain whether this is genuine.
    assert!(m.is_current(old_turn));

    // The endpointing/STT stream proves it's real.
    let out = m.handle(Input::PartialTranscript("No, stop".into()));
    assert_eq!(
        out,
        vec![
            Output::CancelSynthesis(old_turn),
            Output::ShowPartial("No, stop".into()),
            Output::EmitState(SessionState::UserSpeaking),
        ]
    );
    assert_eq!(m.state(), SessionState::UserSpeaking);
    assert!(!m.is_current(old_turn), "old turn is done");
    let new_turn = m.current_turn().expect("new turn for the interruption");
    assert_ne!(new_turn, old_turn);

    // Caller replays exactly the pre-barge-in frames it captured, in order.
    assert_eq!(m.preroll_frames(), expected_preroll);
}

#[test]
fn spurious_barge_in_retracts_after_700ms_of_silence_and_resumes() {
    let (mut m, turn) = to_agent_speaking("what's the weather", "It's sunny today");

    let out = m.handle(Input::BargeInConfirmed);
    assert_eq!(
        out.last(),
        Some(&Output::EmitState(SessionState::Interrupting))
    );

    // Under the window: nothing happens yet.
    let out = m.handle(Input::Tick(Duration::from_millis(300)));
    assert_eq!(out, Vec::<Output>::new());
    let out = m.handle(Input::Tick(Duration::from_millis(399)));
    assert_eq!(out, Vec::<Output>::new());
    assert_eq!(m.state(), SessionState::Interrupting);

    // Crossing 700ms total with no transcript: retract.
    let out = m.handle(Input::Tick(Duration::from_millis(1)));
    assert_eq!(
        out,
        vec![
            Output::ResumeSpeaking,
            Output::EmitState(SessionState::AgentSpeaking)
        ]
    );
    assert_eq!(m.state(), SessionState::AgentSpeaking);
    // The agent's original turn was never cancelled -- it resumes.
    assert!(m.is_current(turn));
}

#[test]
fn agent_finishing_and_barge_in_on_the_same_tick_does_not_deadlock_or_double_cancel() {
    // Order A: PlaybackDrained (with the turn's text already complete)
    // lands before BargeInConfirmed. The turn ends normally; the
    // now-pointless BargeInConfirmed is ignored.
    {
        let (mut m, turn) = to_agent_speaking("play music", "Enjoy the show");
        let _ = m.handle(Input::AgentTurnComplete);

        let drained_out = m.handle(Input::PlaybackDrained);
        assert_eq!(
            drained_out,
            vec![Output::EmitState(SessionState::Listening)]
        );

        let barge_out = m.handle(Input::BargeInConfirmed);
        assert_eq!(barge_out, Vec::<Output>::new(), "nothing left to interrupt");

        assert_eq!(m.state(), SessionState::Listening);
        assert_eq!(m.current_turn(), None);
        assert!(!m.is_current(turn));
    }

    // Order B: BargeInConfirmed lands first. The turn's own drain
    // notification is swallowed while Interrupting (playback was already
    // told to stop), and the turn survives the ambiguity window: exactly
    // one ResumeSpeaking, never a CancelSynthesis, current turn unchanged.
    {
        let (mut m, turn) = to_agent_speaking("play music", "Enjoy the show");
        let _ = m.handle(Input::AgentTurnComplete);

        let mut all_outputs = Vec::new();
        all_outputs.extend(m.handle(Input::BargeInConfirmed));
        assert_eq!(m.state(), SessionState::Interrupting);

        all_outputs.extend(m.handle(Input::PlaybackDrained));
        assert_eq!(
            m.state(),
            SessionState::Interrupting,
            "the late drain notification must not move us out of the uncertainty window"
        );
        assert!(m.is_current(turn));

        all_outputs.extend(m.handle(Input::Tick(Duration::from_millis(700))));
        assert_eq!(m.state(), SessionState::AgentSpeaking);
        assert!(m.is_current(turn), "same turn, never cancelled");

        let cancels: Vec<&Output> = all_outputs
            .iter()
            .filter(|o| matches!(o, Output::CancelSynthesis(_)))
            .collect();
        assert!(
            cancels.is_empty(),
            "never a genuine barge-in, so never cancelled"
        );

        let resumes: Vec<&Output> = all_outputs
            .iter()
            .filter(|o| matches!(o, Output::ResumeSpeaking))
            .collect();
        assert_eq!(
            resumes.len(),
            1,
            "exactly one ResumeSpeaking, not double-fired"
        );
    }
}
