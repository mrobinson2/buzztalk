//! `PreRollBuffer` itself is unit-tested inside the crate; these cover how
//! `SessionMachine` wires it up: only filling while a session is active,
//! and never leaking one session's audio into the next.

use buzztalk_core::FRAME_SAMPLES;
use buzztalk_session::{Input, SessionMachine, SessionState};

fn marker(v: f32) -> [f32; FRAME_SAMPLES] {
    let mut f = [0.0; FRAME_SAMPLES];
    f[0] = v;
    f
}

#[test]
fn frames_pushed_before_session_start_are_dropped() {
    let mut m = SessionMachine::new();
    assert_eq!(m.state(), SessionState::Idle);
    m.push_frame(marker(1.0));
    assert!(m.preroll_frames().is_empty());
}

#[test]
fn frames_pushed_while_active_accumulate_in_order() {
    let mut m = SessionMachine::new();
    let _ = m.handle(Input::SessionStart);
    m.push_frame(marker(1.0));
    m.push_frame(marker(2.0));
    m.push_frame(marker(3.0));

    let got: Vec<f32> = m.preroll_frames().iter().map(|f| f[0]).collect();
    assert_eq!(got, vec![1.0, 2.0, 3.0]);
}

#[test]
fn session_end_clears_the_buffer_for_the_next_session() {
    let mut m = SessionMachine::new();
    let _ = m.handle(Input::SessionStart);
    m.push_frame(marker(1.0));
    m.push_frame(marker(2.0));
    assert_eq!(m.preroll_frames().len(), 2);

    let _ = m.handle(Input::SessionEnd);
    assert!(m.preroll_frames().is_empty());

    // A fresh session starts clean, not haunted by the last one's audio.
    let _ = m.handle(Input::SessionStart);
    assert!(m.preroll_frames().is_empty());
    m.push_frame(marker(9.0));
    assert_eq!(m.preroll_frames(), vec![marker(9.0)]);
}
