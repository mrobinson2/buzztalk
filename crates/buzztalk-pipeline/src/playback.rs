//! Feeding synthesized speech to the duplex engine's playback ring, and the
//! bookkeeping that makes `Output::CancelPlayback`, `Output::CancelSynthesis`,
//! and `Output::ResumeSpeaking` behave the way `buzztalk-session`'s docs
//! promise.
//!
//! ## Why "cancel" doesn't need a real flush API
//!
//! `buzztalk-audio`'s `DuplexEngine`/`PlaybackHandle` expose only
//! `push_playback` -- there is no way to discard samples already sitting in
//! the playback ring once they've been handed over (see this crate's
//! top-level docs for why that's a gap worth closing upstream). This module
//! works around it by never handing the engine more than a small, bounded
//! lead of audio at once ([`JIT_LEAD_FRAMES`]): the orchestration loop tops
//! the ring up by a few frames every tick instead of pushing a whole
//! utterance at once, so "cancel" reduces to "stop topping up", and the
//! residual audio already in the ring drains in a few tens of milliseconds
//! instead of however long the utterance was.
//!
//! ## Turn routing
//!
//! [`PlaybackState`] is the pure part of that story: given the session
//! machine's turn-tagged `Output`s and the TTS worker's turn-tagged audio,
//! it decides what's live, what's paused pending barge-in retraction, and
//! what's been abandoned for good. It touches no clock, no device, and no
//! thread, which is what makes it exhaustively unit-testable without any
//! hardware.

use std::collections::{HashSet, VecDeque};

use buzztalk_session::{Output, SessionState, TurnId};

/// How far ahead of the device the JIT feeder keeps the playback ring
/// filled, in [`buzztalk_core::FRAME_SAMPLES`]-sized frames. Small on
/// purpose: this bounds how much audio can possibly still be sitting in the
/// ring -- and therefore audible -- at the instant playback is cancelled.
pub const JIT_LEAD_FRAMES: usize = 4; // ~40 ms at the 10 ms frame cadence

/// Bookkeeping for what's playing, what's paused (pending barge-in
/// retraction), and what's been permanently abandoned.
#[derive(Debug, Default)]
pub(crate) struct PlaybackState {
    /// Audio queued to feed the engine, JIT, for the turn currently allowed
    /// to be heard.
    live: VecDeque<f32>,
    /// The turn `live` belongs to.
    live_turn: Option<TurnId>,
    /// Set while a turn's audio is paused pending barge-in retraction: the
    /// turn id, plus whatever audio has arrived from the TTS worker since
    /// it was paused (kept, not discarded, so `resume` has something to
    /// resume *with* -- see `Output::ResumeSpeaking`'s contract: resume
    /// from the next un-synthesized chunk, not from silence).
    paused: Option<(TurnId, VecDeque<f32>)>,
    /// Turns whose synthesis was permanently abandoned
    /// (`Output::CancelSynthesis`) -- late TTS results for these are
    /// dropped rather than played.
    abandoned: HashSet<TurnId>,
    /// Samples dropped because they arrived for a turn this state no longer
    /// recognises (already abandoned, or never started).
    stale_chunks_dropped: u64,
}

impl PlaybackState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A new turn's audio starts flowing: the first chunk of a fresh
    /// `AgentSpeaking`. Replaces whatever was live (there should be
    /// nothing, by construction -- only one agent turn plays at a time).
    pub(crate) fn start(&mut self, turn: TurnId) {
        self.live.clear();
        self.live_turn = Some(turn);
        self.abandoned.remove(&turn);
    }

    /// `Output::CancelPlayback`: stop what's live *now*. Parks the
    /// interrupted turn (audio-preserving) so a later
    /// [`Self::resume`] can pick it back up without having lost anything.
    /// Returns the turn that was interrupted, if any.
    pub(crate) fn cancel_playback(&mut self) -> Option<TurnId> {
        let turn = self.live_turn.take()?;
        self.live.clear();
        self.paused = Some((turn, VecDeque::new()));
        Some(turn)
    }

    /// `Output::FlushOutputBuffer`: discard whatever's buffered-but-unplayed.
    /// Idempotent with [`Self::cancel_playback`] (which already clears
    /// `live`); kept as a separate method because they're separate
    /// `Output` variants and the session machine doesn't always pair them
    /// with an intervening event.
    pub(crate) fn flush(&mut self) {
        self.live.clear();
    }

    /// `Output::CancelSynthesis(turn)`: abandon `turn` for good. Drops
    /// whatever paused audio it had and marks it so late TTS results are
    /// discarded rather than played. This is the only "stop for good"
    /// signal -- see `buzztalk_session::SessionMachine::is_current`'s docs.
    pub(crate) fn cancel_synthesis(&mut self, turn: TurnId) {
        self.abandoned.insert(turn);
        if self.is_live(turn) {
            self.live_turn = None;
            self.live.clear();
        }
        if matches!(&self.paused, Some((t, _)) if *t == turn) {
            self.paused = None;
        }
    }

    /// `Output::ResumeSpeaking(turn)`: a barge-in retracted as spurious.
    /// Whatever was parked for `turn` becomes live again. Returns `true` if
    /// there was something parked for exactly this turn to resume.
    pub(crate) fn resume(&mut self, turn: TurnId) -> bool {
        match self.paused.take() {
            Some((t, queued)) if t == turn => {
                self.live_turn = Some(turn);
                self.live = queued;
                true
            }
            other => {
                self.paused = other;
                false
            }
        }
    }

    /// Route one synthesized chunk from the TTS worker: onto `live` if it
    /// belongs to the turn currently playing, into the paused buffer if
    /// that turn is paused, or dropped (counted) if it belongs to a turn
    /// this state no longer recognises.
    pub(crate) fn route_synthesized(&mut self, turn: TurnId, samples: &[f32]) {
        if self.abandoned.contains(&turn) {
            self.stale_chunks_dropped += 1;
            return;
        }
        if self.is_live(turn) {
            self.live.extend(samples.iter().copied());
            return;
        }
        if let Some((t, queued)) = &mut self.paused {
            if *t == turn {
                queued.extend(samples.iter().copied());
                return;
            }
        }
        self.stale_chunks_dropped += 1;
    }

    /// Pop up to `max_samples` off the front of `live` to feed the engine
    /// this tick.
    pub(crate) fn take_live(&mut self, max_samples: usize) -> Vec<f32> {
        let n = self.live.len().min(max_samples);
        self.live.drain(..n).collect()
    }

    /// Whether `turn` is the one currently allowed to be heard.
    pub(crate) fn is_live(&self, turn: TurnId) -> bool {
        self.live_turn == Some(turn)
    }

    /// Whether some turn is recognised (live, or paused pending
    /// retraction) -- i.e. whether a caller should bother trying to feed
    /// this turn's text to synthesis at all.
    pub(crate) fn recognises(&self, turn: TurnId) -> bool {
        self.live_turn == Some(turn) || matches!(&self.paused, Some((t, _)) if *t == turn)
    }

    /// How many samples of `live` are still buffered pipeline-side, waiting
    /// to be fed to the engine.
    pub(crate) fn live_len(&self) -> usize {
        self.live.len()
    }

    /// Total chunks dropped for turns this state no longer recognised.
    pub(crate) fn stale_chunks_dropped(&self) -> u64 {
        self.stale_chunks_dropped
    }
}

/// What the orchestrator should actually *do* in response to one `Output`,
/// decoupled from any real audio device or worker thread so it can be unit
/// tested directly.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct OutputActions {
    pub(crate) start_capture: bool,
    pub(crate) stop_capture: bool,
    pub(crate) show_partial: Option<String>,
    pub(crate) submit_utterance: Option<(TurnId, String)>,
    pub(crate) cancel_synthesis: Option<TurnId>,
    pub(crate) replay_preroll: Option<TurnId>,
    pub(crate) resumed_turn: Option<TurnId>,
    pub(crate) state_changed: Option<SessionState>,
    /// Whether this `Output` caused a turn's playback to actually be
    /// interrupted (as opposed to a no-op cancel with nothing live) -- used
    /// to arm the barge-in-to-silence metric.
    pub(crate) playback_cancelled: bool,
}

/// Translate one `Output` into [`OutputActions`], applying its turn-routing
/// side effects to `playback` along the way. Pure aside from that one piece
/// of owned state -- no I/O, no threads, no clock -- so this is
/// exhaustively unit-testable against a real `SessionMachine`'s output
/// without any hardware or model on disk.
pub(crate) fn route_output(output: &Output, playback: &mut PlaybackState) -> OutputActions {
    let mut actions = OutputActions::default();
    match output {
        Output::StartCapture => actions.start_capture = true,
        Output::StopCapture => actions.stop_capture = true,
        Output::ShowPartial(text) => actions.show_partial = Some(text.clone()),
        Output::SubmitUtterance { turn, text } => {
            actions.submit_utterance = Some((*turn, text.clone()));
        }
        Output::CancelPlayback => {
            actions.playback_cancelled = playback.cancel_playback().is_some();
        }
        Output::FlushOutputBuffer => playback.flush(),
        Output::CancelSynthesis(turn) => {
            playback.cancel_synthesis(*turn);
            actions.cancel_synthesis = Some(*turn);
        }
        Output::ReplayPreRoll(turn) => actions.replay_preroll = Some(*turn),
        Output::ResumeSpeaking(turn) => {
            if playback.resume(*turn) {
                actions.resumed_turn = Some(*turn);
            }
        }
        Output::EmitState(state) => actions.state_changed = Some(*state),
    }
    actions
}

/// Upsample `samples` (mono `f32` at `from_hz`) to
/// [`buzztalk_core::SAMPLE_RATE_HZ`] via linear interpolation.
///
/// Used two ways: Pocket TTS emits 24 kHz and the duplex engine's playback
/// ring only accepts 48 kHz (see `buzztalk_audio::DuplexEngine::push_playback`'s
/// docs); and `--simulate` mode's WAV capture is typically 16 kHz and needs
/// to arrive shaped like real 48 kHz capture. Both are small integer
/// ratios, for which linear interpolation is a fine trade for speech.
/// There is no sibling-crate resampler this crate can reuse for either
/// direction: `buzztalk-stt`'s `Resampler48to16` only goes 48 -> 16, and
/// `buzztalk-audio`'s internal resampler is private to that crate.
pub(crate) fn resample_linear_to_48k(samples: &[f32], from_hz: u32) -> Vec<f32> {
    if from_hz == 0 || samples.is_empty() {
        return Vec::new();
    }
    if from_hz == buzztalk_core::SAMPLE_RATE_HZ {
        return samples.to_vec();
    }
    let ratio = buzztalk_core::SAMPLE_RATE_HZ as f64 / from_hz as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let last_idx = samples.len() - 1;
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx0 = (src_pos.floor() as usize).min(last_idx);
        let frac = (src_pos - idx0 as f64) as f32;
        let s0 = samples[idx0];
        let s1 = samples[(idx0 + 1).min(last_idx)];
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_session::{Input, SessionMachine};

    /// Mint distinct real `TurnId`s the only way one is obtainable outside
    /// `buzztalk-session`: drive an actual `SessionMachine` through
    /// push-to-talk turns.
    fn turns(n: usize) -> Vec<TurnId> {
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        let mut ids = Vec::new();
        for _ in 0..n {
            machine.handle(Input::PushToTalkPressed);
            ids.push(machine.current_turn().unwrap());
            machine.handle(Input::PushToTalkReleased);
            let turn = *ids.last().unwrap();
            machine.handle(Input::FinalTranscript {
                turn,
                text: "hi".into(),
            });
            machine.handle(Input::SubmitSucceeded { turn });
            machine.handle(Input::AgentTurnComplete { turn });
        }
        ids
    }

    // ── PlaybackState ───────────────────────────────────────────────────

    #[test]
    fn fresh_state_recognises_nothing() {
        let [t] = turns(1)[..] else { unreachable!() };
        let state = PlaybackState::new();
        assert!(!state.is_live(t));
        assert!(!state.recognises(t));
    }

    #[test]
    fn start_makes_a_turn_live_and_takeable() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.route_synthesized(t, &[1.0, 2.0, 3.0]);
        assert!(state.is_live(t));
        assert_eq!(state.live_len(), 3);
        assert_eq!(state.take_live(2), vec![1.0, 2.0]);
        assert_eq!(state.live_len(), 1);
    }

    #[test]
    fn cancel_playback_clears_live_audio_and_parks_the_turn() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.route_synthesized(t, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(state.live_len(), 4);

        let interrupted = state.cancel_playback();
        assert_eq!(interrupted, Some(t));
        // The whole point of the JIT-feed design: cancel actually empties
        // the pipeline-side buffer, not just "stop adding to it".
        assert_eq!(state.live_len(), 0);
        assert!(!state.is_live(t));
        // But the turn isn't gone -- it's parked, still recognised.
        assert!(state.recognises(t));
    }

    #[test]
    fn cancel_playback_with_nothing_live_is_a_reported_no_op() {
        let mut state = PlaybackState::new();
        assert_eq!(state.cancel_playback(), None);
    }

    #[test]
    fn flush_clears_live_without_touching_pause_state() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.route_synthesized(t, &[1.0, 2.0]);
        state.flush();
        assert_eq!(state.live_len(), 0);
        assert!(
            state.is_live(t),
            "flush must not un-live the turn, only empty its buffer"
        );
    }

    #[test]
    fn audio_synthesized_while_paused_is_kept_not_dropped() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.cancel_playback();
        // TTS worker keeps producing audio for the parked turn while the
        // session machine hasn't decided yet whether the barge-in is real.
        state.route_synthesized(t, &[9.0, 9.0]);
        assert_eq!(state.stale_chunks_dropped(), 0);

        assert!(state.resume(t));
        assert_eq!(state.take_live(2), vec![9.0, 9.0]);
    }

    #[test]
    fn resume_for_the_wrong_turn_does_nothing() {
        let ids = turns(2);
        let (a, b) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(a);
        state.cancel_playback();

        assert!(
            !state.resume(b),
            "resuming a turn that isn't the paused one must fail"
        );
        assert!(
            state.recognises(a),
            "the actually-paused turn must still be there"
        );
    }

    #[test]
    fn cancel_synthesis_drops_paused_audio_and_marks_the_turn_abandoned() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.cancel_playback();
        state.route_synthesized(t, &[1.0, 2.0, 3.0]);

        state.cancel_synthesis(t);
        assert!(!state.recognises(t));
        assert!(!state.resume(t), "an abandoned turn cannot be resumed");

        // Late TTS result for the now-abandoned turn: dropped, not played.
        state.route_synthesized(t, &[5.0]);
        assert_eq!(state.stale_chunks_dropped(), 1);
    }

    #[test]
    fn cancel_synthesis_on_the_live_turn_stops_it_outright() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.route_synthesized(t, &[1.0, 2.0]);

        state.cancel_synthesis(t);
        assert!(!state.is_live(t));
        assert_eq!(state.live_len(), 0);
    }

    #[test]
    fn stale_chunk_for_an_unknown_turn_is_dropped_and_counted() {
        let ids = turns(2);
        let (a, b) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(a);
        // b was never started here at all.
        state.route_synthesized(b, &[1.0]);
        assert_eq!(state.stale_chunks_dropped(), 1);
        assert_eq!(state.live_len(), 0);
    }

    #[test]
    fn a_second_start_replaces_the_first_turn_cleanly() {
        let ids = turns(2);
        let (a, b) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(a);
        state.route_synthesized(a, &[1.0, 2.0]);
        state.start(b);
        assert!(!state.is_live(a));
        assert!(state.is_live(b));
        assert_eq!(state.live_len(), 0);
    }

    // ── route_output: the Output -> action mapping ──────────────────────

    #[test]
    fn cancel_playback_output_reports_interruption_only_when_something_was_live() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();

        let actions = route_output(&Output::CancelPlayback, &mut state);
        assert!(!actions.playback_cancelled, "nothing was live yet");

        state.start(t);
        let actions = route_output(&Output::CancelPlayback, &mut state);
        assert!(actions.playback_cancelled);
    }

    #[test]
    fn flush_output_buffer_maps_to_a_flush_with_no_flagged_action() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);
        state.route_synthesized(t, &[1.0, 2.0]);

        let actions = route_output(&Output::FlushOutputBuffer, &mut state);
        assert_eq!(actions, OutputActions::default());
        assert_eq!(state.live_len(), 0);
    }

    #[test]
    fn cancel_synthesis_output_carries_the_turn_and_abandons_it() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        state.start(t);

        let actions = route_output(&Output::CancelSynthesis(t), &mut state);
        assert_eq!(actions.cancel_synthesis, Some(t));
        assert!(!state.recognises(t));
    }

    #[test]
    fn replay_preroll_output_carries_the_new_candidate_turn() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        let actions = route_output(&Output::ReplayPreRoll(t), &mut state);
        assert_eq!(actions.replay_preroll, Some(t));
    }

    #[test]
    fn resume_speaking_output_reports_resumption_only_if_that_turn_was_paused() {
        let ids = turns(2);
        let (a, b) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(a);
        state.cancel_playback();

        let actions = route_output(&Output::ResumeSpeaking(b), &mut state);
        assert_eq!(actions.resumed_turn, None, "b was never paused");

        let actions = route_output(&Output::ResumeSpeaking(a), &mut state);
        assert_eq!(actions.resumed_turn, Some(a));
        assert!(state.is_live(a));
    }

    #[test]
    fn emit_state_output_surfaces_the_new_state() {
        let mut state = PlaybackState::new();
        let actions = route_output(&Output::EmitState(SessionState::Listening), &mut state);
        assert_eq!(actions.state_changed, Some(SessionState::Listening));
    }

    #[test]
    fn submit_utterance_output_carries_turn_and_text() {
        let [t] = turns(1)[..] else { unreachable!() };
        let mut state = PlaybackState::new();
        let actions = route_output(
            &Output::SubmitUtterance {
                turn: t,
                text: "hello there".into(),
            },
            &mut state,
        );
        assert_eq!(actions.submit_utterance, Some((t, "hello there".into())));
    }

    #[test]
    fn start_and_stop_capture_outputs_map_to_flags() {
        let mut state = PlaybackState::new();
        assert!(route_output(&Output::StartCapture, &mut state).start_capture);
        assert!(route_output(&Output::StopCapture, &mut state).stop_capture);
    }

    /// End-to-end through `route_output` for the exact `Output` sequence
    /// `SessionMachine::on_barge_in_confirmed` documents:
    /// `[CancelPlayback, FlushOutputBuffer, ReplayPreRoll(candidate)]`,
    /// followed later by either `CancelSynthesis` (confirmed genuine) or
    /// `ResumeSpeaking` (retracted).
    #[test]
    fn full_barge_in_then_genuine_confirmation_sequence() {
        let ids = turns(2);
        let (agent_turn, candidate) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(agent_turn);
        state.route_synthesized(agent_turn, &[1.0, 2.0, 3.0]);

        for output in [
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::ReplayPreRoll(candidate),
        ] {
            route_output(&output, &mut state);
        }
        assert_eq!(state.live_len(), 0);
        assert!(
            state.recognises(agent_turn),
            "still parked, not yet cancelled"
        );

        // TTS keeps producing audio for the parked agent turn while
        // unconfirmed.
        state.route_synthesized(agent_turn, &[7.0]);

        // Proven genuine: the parked turn is cancelled for good.
        let actions = route_output(&Output::CancelSynthesis(agent_turn), &mut state);
        assert_eq!(actions.cancel_synthesis, Some(agent_turn));
        assert!(!state.recognises(agent_turn));
    }

    #[test]
    fn full_barge_in_then_spurious_retraction_resumes_with_no_audio_lost() {
        let ids = turns(2);
        let (agent_turn, candidate) = (ids[0], ids[1]);
        let mut state = PlaybackState::new();
        state.start(agent_turn);
        state.route_synthesized(agent_turn, &[1.0, 2.0]);

        for output in [
            Output::CancelPlayback,
            Output::FlushOutputBuffer,
            Output::ReplayPreRoll(candidate),
        ] {
            route_output(&output, &mut state);
        }
        // More of the agent's reply keeps synthesizing while parked.
        state.route_synthesized(agent_turn, &[3.0, 4.0]);

        let actions = route_output(&Output::ResumeSpeaking(agent_turn), &mut state);
        assert_eq!(actions.resumed_turn, Some(agent_turn));
        assert!(state.is_live(agent_turn));
        // Nothing synthesized while paused was lost.
        assert_eq!(state.take_live(10), vec![3.0, 4.0]);
    }

    // ── resample_linear_to_48k ───────────────────────────────────────────

    #[test]
    fn resample_at_native_rate_is_a_passthrough() {
        let input = vec![0.1, 0.2, 0.3];
        let out = resample_linear_to_48k(&input, buzztalk_core::SAMPLE_RATE_HZ);
        assert_eq!(out, input);
    }

    #[test]
    fn resample_24k_to_48k_doubles_length() {
        let input: Vec<f32> = (0..240).map(|i| (i as f32 * 0.1).sin()).collect();
        let out = resample_linear_to_48k(&input, 24_000);
        assert_eq!(out.len(), 480);
    }

    #[test]
    fn resample_16k_to_48k_triples_length() {
        let input = vec![0.0_f32; 160];
        let out = resample_linear_to_48k(&input, 16_000);
        assert_eq!(out.len(), 480);
    }

    #[test]
    fn resample_preserves_a_constant_signal() {
        let input = vec![0.5_f32; 100];
        let out = resample_linear_to_48k(&input, 24_000);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn resample_empty_input_yields_empty_output() {
        assert!(resample_linear_to_48k(&[], 24_000).is_empty());
    }
}
