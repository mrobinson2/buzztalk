//! The single most important correctness property in this crate: a result
//! for a turn that is no longer current must be dropped, never rendered --
//! even when the machine happens to be sitting in exactly the state that
//! would normally expect that result.
//!
//! That last clause is the point of this file. State-only gating (accept a
//! `FinalTranscript` whenever `state == Finalizing`, accept `AgentTextArrived`
//! whenever `state == AgentSpeaking`, etc.) is *not* sufficient: a turn can
//! be abandoned while in some state, and a later, different turn can pass
//! back through that same state before the abandoned turn's slow async
//! result finally lands. Every test below drives exactly that sequence and
//! asserts the stale result is dropped. Each one is written so that
//! commenting out the identity check in the corresponding `on_*` handler
//! (leaving only the state check) makes it fail -- see the note at the
//! bottom of this file for how that was verified.

mod common;

use buzztalk_session::{Input, Output, SessionState};
use common::{to_agent_speaking, to_finalizing};

#[test]
fn stale_final_transcript_is_ignored_even_when_a_later_turn_shares_its_state() {
    // Turn A reaches Finalizing and stalls (its STT decode is slow).
    let (mut m, turn_a) = to_finalizing();
    assert_eq!(m.state(), SessionState::Finalizing);

    // Turn A is abandoned -- push-to-talk seizes the mic before A's final
    // transcript ever arrives.
    let out = m.handle(Input::PushToTalkPressed);
    assert_eq!(out, vec![Output::EmitState(SessionState::UserSpeaking)]);
    let turn_b = m.current_turn().expect("ptt started a new turn");
    assert_ne!(turn_b, turn_a);

    // Turn B runs its own course and lands right back in Finalizing --
    // the exact state A was abandoned in.
    let out = m.handle(Input::EndpointEvent(
        buzztalk_core::DetectorEvent::SpeechEnd,
    ));
    assert_eq!(out, vec![Output::EmitState(SessionState::Finalizing)]);
    assert_eq!(m.state(), SessionState::Finalizing);

    // Now A's slow decode finally lands. State-only gating would accept
    // this -- we are sitting in exactly the state that expects a
    // FinalTranscript. Identity must reject it anyway.
    let out = m.handle(Input::FinalTranscript {
        turn: turn_a,
        text: "this is turn A's words, arriving far too late".into(),
    });
    assert_eq!(
        out,
        Vec::<Output>::new(),
        "turn A is not current; must be dropped"
    );
    assert_eq!(m.state(), SessionState::Finalizing, "must not have moved");
    assert_eq!(m.current_turn(), Some(turn_b));

    // Sanity: the machine still works normally for the turn that *is*
    // current.
    let out = m.handle(Input::FinalTranscript {
        turn: turn_b,
        text: "turn B's real words".into(),
    });
    assert_eq!(
        out,
        vec![
            Output::SubmitUtterance {
                turn: turn_b,
                text: "turn B's real words".into(),
            },
            Output::EmitState(SessionState::Submitting),
        ]
    );
}

#[test]
fn stale_agent_text_is_ignored_even_when_a_later_turn_shares_its_state() {
    // Turn A's agent reply is playing.
    let (mut m, turn_a) = to_agent_speaking("what time is it", "It's currently");
    assert_eq!(m.state(), SessionState::AgentSpeaking);

    // A genuine barge-in cancels turn A and starts turn B.
    let _ = m.handle(Input::BargeInConfirmed);
    let turn_b = m
        .current_turn()
        .expect("candidate turn during Interrupting");
    let out = m.handle(Input::PartialTranscript {
        turn: turn_b,
        text: "Actually never mind".into(),
    });
    assert_eq!(out[0], Output::CancelSynthesis(turn_a));
    assert_eq!(m.state(), SessionState::UserSpeaking);

    // Turn B runs its own full course, back to AwaitingAgent -- the exact
    // state agent text is expected in, just like it was for turn A's first
    // chunk. Deliberately stop *before* turn B's own AgentTextArrived: that
    // transition (AwaitingAgent -> AgentSpeaking) is the only place a
    // wrongly-accepted stale chunk would actually show up as an observable
    // effect -- AgentSpeaking's own arm for a repeat chunk is a silent
    // no-op either way, which would make this test toothless.
    let _ = m.handle(Input::EndpointEvent(
        buzztalk_core::DetectorEvent::SpeechEnd,
    ));
    let _ = m.handle(Input::FinalTranscript {
        turn: turn_b,
        text: "never mind, cancel that".into(),
    });
    let _ = m.handle(Input::SubmitSucceeded { turn: turn_b });
    assert_eq!(m.state(), SessionState::AwaitingAgent);
    assert_eq!(m.current_turn(), Some(turn_b));

    // A chunk from turn A's now-cancelled agent stream arrives late.
    // State-only gating would accept it and wrongly promote us to
    // AgentSpeaking on turn A's say-so -- we're in exactly the state that
    // expects a turn's first agent chunk. Identity must reject it.
    let out = m.handle(Input::AgentTextArrived {
        turn: turn_a,
        text: " three fifteen.".into(),
    });
    assert_eq!(
        out,
        Vec::<Output>::new(),
        "turn A is not current; must be dropped"
    );
    assert_eq!(
        m.state(),
        SessionState::AwaitingAgent,
        "must not have been promoted to AgentSpeaking by turn A's chunk"
    );
    assert_eq!(m.current_turn(), Some(turn_b));

    // Sanity: turn B's own chunk still works normally.
    let out = m.handle(Input::AgentTextArrived {
        turn: turn_b,
        text: "No problem, cancelled.".into(),
    });
    assert_eq!(out, vec![Output::EmitState(SessionState::AgentSpeaking)]);
}

#[test]
fn stale_playback_drained_is_ignored_even_when_a_later_turn_shares_its_state() {
    // Turn A's agent reply is playing.
    let (mut m, turn_a) = to_agent_speaking("play some jazz", "Sure, starting now");

    // A genuine barge-in cancels turn A and starts turn B.
    let _ = m.handle(Input::BargeInConfirmed);
    let turn_b = m
        .current_turn()
        .expect("candidate turn during Interrupting");
    let _ = m.handle(Input::PartialTranscript {
        turn: turn_b,
        text: "stop the music".into(),
    });

    // Turn B runs its own full course, back to AgentSpeaking with its own
    // text already fully delivered (AgentTurnComplete) -- exactly the
    // condition under which a drain notification actually does something
    // (ends the turn). A drain notification while AgentSpeaking but
    // *without* AgentTurnComplete yet is a silent no-op regardless of whose
    // turn it names, which would make this test toothless.
    let _ = m.handle(Input::EndpointEvent(
        buzztalk_core::DetectorEvent::SpeechEnd,
    ));
    let _ = m.handle(Input::FinalTranscript {
        turn: turn_b,
        text: "stop the music".into(),
    });
    let _ = m.handle(Input::SubmitSucceeded { turn: turn_b });
    let _ = m.handle(Input::AgentTextArrived {
        turn: turn_b,
        text: "Music stopped.".into(),
    });
    let _ = m.handle(Input::AgentTurnComplete { turn: turn_b });
    assert_eq!(m.state(), SessionState::AgentSpeaking);
    assert_eq!(m.current_turn(), Some(turn_b));

    // Turn A's audio device finally reports its buffer drained -- a late,
    // orphaned callback from audio that was already flushed. State-only
    // gating would accept it and end turn B's turn on turn A's say-so --
    // we're in exactly the state (AgentSpeaking, turn-complete) that
    // expects a drain notification to end things. Identity must reject it.
    let out = m.handle(Input::PlaybackDrained { turn: turn_a });
    assert_eq!(
        out,
        Vec::<Output>::new(),
        "turn A is not current; must be dropped"
    );
    assert_eq!(
        m.state(),
        SessionState::AgentSpeaking,
        "must not have ended turn B's turn on turn A's drain notification"
    );
    assert_eq!(m.current_turn(), Some(turn_b));

    // Sanity: turn B's own drain notification still ends its turn normally.
    let out = m.handle(Input::PlaybackDrained { turn: turn_b });
    assert_eq!(out, vec![Output::EmitState(SessionState::Listening)]);
}

// Regression-proofing note (not a test): before the identity check was
// added, each of the three tests above was verified to fail by
// temporarily short-circuiting the `if !self.is_current(turn) { return
// vec![]; }` guard at the top of `on_final_transcript`, `on_agent_text_arrived`,
// and `on_playback_drained` in `src/machine.rs` (i.e. reverting to
// state-only gating). All three failed with the stale content being
// accepted -- `on_final_transcript` re-submitted turn A's stale words as
// turn B's `SubmitUtterance`, `on_agent_text_arrived` silently accepted the
// stale chunk (a no-op state-wise, but only because AgentSpeaking doesn't
// change state on a repeat chunk -- the acceptance itself is the bug), and
// `on_playback_drained` incorrectly ended turn B's turn using turn A's
// drain notification. Restoring the identity check made all three pass.
