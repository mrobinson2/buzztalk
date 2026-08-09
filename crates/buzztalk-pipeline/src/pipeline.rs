//! [`ConversationPipeline`]: owns the orchestration thread plus the STT and
//! TTS worker threads, and wires everything sibling crates provide into an
//! actual, interruptible conversation. See the crate-level docs for the
//! architecture and the design rationale behind the trickier parts
//! ([`crate::playback`] for cancellation, [`crate::agent`] for the test
//! backend).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use buzztalk_aec::new_best_available;
use buzztalk_audio::{detect_output_route, DuplexConfig, DuplexEngine};
use buzztalk_core::{DetectorEvent, EchoCanceller, OutputRoute, SpeechDetector, FRAME_SAMPLES};
use buzztalk_session::{Frame, Input, Output, SessionMachine, SessionState, TurnId};
use buzztalk_stt::Resampler48to16;
use buzztalk_vad::{BargeInDetector, EndpointDetector};

use crate::agent::{AgentBackend, AgentEvent, EchoAgent};
use crate::capture;
use crate::error::PipelineError;
use crate::metrics::TurnMetrics;
use crate::playback::{route_output, PlaybackState, JIT_LEAD_FRAMES};
use crate::stt_worker::{SttResult, SttWorker};
use crate::tts_worker::TtsWorker;

/// How often the orchestration loop wakes up to check for new capture
/// frames, worker results, and control messages. Short enough to keep the
/// playback JIT lead ([`JIT_LEAD_FRAMES`]) topped up smoothly; matches the
/// cadence `buzztalk-labs`' harnesses already use for the same reason.
const LOOP_SLEEP: Duration = Duration::from_millis(5);

/// Configuration for [`ConversationPipeline::start`].
pub struct PipelineConfig {
    /// Passed straight through to `DuplexEngine::start`.
    pub duplex: DuplexConfig,
    /// If set, overrides real output-route detection -- e.g. `--headphones`
    /// on a machine (like this dev box) that reports [`OutputRoute::Unknown`],
    /// which would otherwise gate barge-in as if an echo path were present.
    pub forced_output_route: Option<OutputRoute>,
    /// If set, microphone capture is replaced with frames decoded from this
    /// WAV file (see [`crate::capture`]) instead of the real device -- for
    /// machines with no usable microphone input.
    pub simulate_capture: Option<PathBuf>,
    /// Whatever produces the agent's replies.
    pub agent: Box<dyn AgentBackend>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            duplex: DuplexConfig::default(),
            forced_output_route: None,
            simulate_capture: None,
            agent: Box::new(EchoAgent::new()),
        }
    }
}

/// Events the orchestration thread reports outward -- e.g. to a terminal
/// demo or a UI.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// The session's phase just changed.
    StateChanged(SessionState),
    /// An interim transcript for the user's in-progress utterance.
    Partial(String),
    /// The finalized transcript for the user's utterance.
    FinalTranscript(String),
    /// A chunk of the agent's reply text.
    AgentText(String),
    /// A pre-formatted latency summary for the turn that just ended.
    TurnMetrics(String),
    /// Something was dropped under backpressure (drop-instead-of-block
    /// paths, see the crate's top-level docs).
    Dropped {
        /// What kind of thing was dropped.
        what: &'static str,
        /// Running total dropped so far, of this kind.
        total: u64,
    },
    /// The session ended and the orchestration loop is shutting down.
    SessionEnded,
}

/// Control messages sent into the orchestration thread.
enum PipelineControl {
    StartSession,
    EndSession,
    PushToTalkPressed,
    PushToTalkReleased,
    Shutdown,
}

/// The conversation pipeline: owns the orchestration thread (audio pump,
/// AEC, both VAD detectors, the session machine, and playback feeding) and
/// the STT/TTS worker threads, wired together end to end.
///
/// Dropping this shuts everything down: the orchestration thread is told to
/// stop and joined, which in turn drops the STT/TTS workers, joining their
/// threads too.
pub struct ConversationPipeline {
    control_tx: Sender<PipelineControl>,
    events_rx: Receiver<PipelineEvent>,
    orchestrator: Option<JoinHandle<()>>,
}

impl ConversationPipeline {
    /// Build every component that can fail, then spawn the orchestration
    /// thread. Construction errors (a missing model, a device that won't
    /// open) surface synchronously here rather than being swallowed inside
    /// a background thread.
    pub fn start(config: PipelineConfig) -> Result<Self, PipelineError> {
        let PipelineConfig {
            duplex,
            forced_output_route,
            simulate_capture,
            agent,
        } = config;

        let engine = DuplexEngine::start(duplex)?;
        let aec = new_best_available();
        let stt = SttWorker::spawn()?;
        let tts = TtsWorker::spawn()?;
        let simulate_frames = match &simulate_capture {
            Some(path) => Some(capture::load_as_capture_frames(path)?),
            None => None,
        };

        let (control_tx, control_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();

        let mut orchestrator = Orchestrator {
            engine,
            aec,
            endpoint: EndpointDetector::default(),
            bargein: BargeInDetector::new(Default::default()),
            session: SessionMachine::new(),
            resampler_16k: Resampler48to16::new(),
            stt,
            tts,
            agent,
            playback: PlaybackState::new(),
            forced_output_route,
            simulate_frames,
            sim_cursor: 0,
            sim_start: None,
            capturing: false,
            stt_active_turn: None,
            pending_ref: VecDeque::new(),
            pending_cap: VecDeque::new(),
            playback_pushed_samples: 0,
            playback_push_started_at: None,
            metrics: TurnMetrics::new(),
            silence_watch: None,
            stale_chunks_reported: 0,
            events_tx,
            control_rx,
            running: true,
        };

        let handle = thread::Builder::new()
            .name("buzztalk-orchestrator".into())
            .spawn(move || orchestrator.run())
            .expect("failed to spawn buzztalk-pipeline orchestrator thread");

        Ok(Self {
            control_tx,
            events_rx,
            orchestrator: Some(handle),
        })
    }

    /// Start a new session: opens capture and begins listening.
    pub fn start_session(&self) {
        let _ = self.control_tx.send(PipelineControl::StartSession);
    }

    /// End the current session gracefully.
    pub fn end_session(&self) {
        let _ = self.control_tx.send(PipelineControl::EndSession);
    }

    /// Push-to-talk pressed: seize the turn for the user, interrupting the
    /// agent if it was speaking.
    pub fn push_to_talk_pressed(&self) {
        let _ = self.control_tx.send(PipelineControl::PushToTalkPressed);
    }

    /// Push-to-talk released: end the user's utterance and finalize it.
    pub fn push_to_talk_released(&self) {
        let _ = self.control_tx.send(PipelineControl::PushToTalkReleased);
    }

    /// Non-blocking poll for the next pipeline event.
    pub fn try_recv_event(&self) -> Option<PipelineEvent> {
        self.events_rx.try_recv().ok()
    }

    /// Block for up to `timeout` for the next pipeline event.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<PipelineEvent> {
        self.events_rx.recv_timeout(timeout).ok()
    }
}

impl Drop for ConversationPipeline {
    fn drop(&mut self) {
        let _ = self.control_tx.send(PipelineControl::Shutdown);
        if let Some(handle) = self.orchestrator.take() {
            let _ = handle.join();
        }
    }
}

/// Owns every piece of mutable state the orchestration loop touches. Split
/// out from [`ConversationPipeline`] so the loop itself (`run` and its
/// helpers) reads as one cohesive unit instead of a closure capturing a
/// dozen loose variables.
struct Orchestrator {
    engine: DuplexEngine,
    aec: Box<dyn EchoCanceller>,
    endpoint: EndpointDetector,
    bargein: BargeInDetector,
    session: SessionMachine,
    resampler_16k: Resampler48to16,
    stt: SttWorker,
    tts: TtsWorker,
    agent: Box<dyn AgentBackend>,
    playback: PlaybackState,

    forced_output_route: Option<OutputRoute>,

    /// `--simulate` support: pre-chunked 48 kHz frames decoded from a WAV
    /// file, delivered in place of real capture, paced to real time.
    simulate_frames: Option<Vec<Vec<f32>>>,
    sim_cursor: usize,
    sim_start: Option<Instant>,

    capturing: bool,
    /// The turn currently being fed audio for transcription (the user's
    /// in-progress utterance, live or a confirmed/candidate barge-in).
    stt_active_turn: Option<TurnId>,

    /// Render-reference / capture frames pulled from the engine (or, in
    /// simulate mode, from the WAV), paired up before AEC processing --
    /// mirrors the pattern `buzztalk-labs`' `live-aec` harness uses.
    pending_ref: VecDeque<Vec<f32>>,
    pending_cap: VecDeque<Vec<f32>>,

    /// Cumulative samples handed to `engine.push_playback` since the
    /// pacing anchor below was last reset -- the JIT feeder's "how much is
    /// probably still resident in the ring" estimate. See
    /// [`Orchestrator::feed_playback`].
    playback_pushed_samples: u64,
    playback_push_started_at: Option<Instant>,

    metrics: TurnMetrics,
    /// Armed on a genuine barge-in detection with the engine's underrun
    /// counter at that instant; cleared once a fresh underrun proves
    /// playback actually went silent.
    silence_watch: Option<u64>,
    /// Last value of `playback.stale_chunks_dropped()` this loop already
    /// reported, so [`PipelineEvent::Dropped`] is only emitted when the
    /// count actually moves.
    stale_chunks_reported: u64,

    events_tx: Sender<PipelineEvent>,
    control_rx: Receiver<PipelineControl>,
    running: bool,
}

impl Orchestrator {
    fn run(&mut self) {
        let mut last_tick = Instant::now();
        while self.running {
            while let Ok(ctrl) = self.control_rx.try_recv() {
                match ctrl {
                    PipelineControl::StartSession => self.dispatch(Input::SessionStart),
                    PipelineControl::EndSession => self.dispatch(Input::SessionEnd),
                    PipelineControl::PushToTalkPressed => self.dispatch(Input::PushToTalkPressed),
                    PipelineControl::PushToTalkReleased => self.dispatch(Input::PushToTalkReleased),
                    PipelineControl::Shutdown => self.running = false,
                }
            }
            if !self.running {
                break;
            }

            let now = Instant::now();
            let dt = now.duration_since(last_tick);
            last_tick = now;
            self.dispatch(Input::Tick(dt));

            self.pump_audio();

            while let Some(result) = self.stt.try_recv_result() {
                self.handle_stt_result(result);
            }

            while let Some(event) = self.agent.poll() {
                self.handle_agent_event(event);
            }

            while let Some(result) = self.tts.try_recv_result() {
                self.playback
                    .route_synthesized(result.turn, &result.samples_48k);
            }
            self.report_stale_chunks();

            self.feed_playback();
            self.check_silence_watch();

            thread::sleep(LOOP_SLEEP);
        }

        self.dispatch(Input::SessionEnd);
        let _ = self.events_tx.send(PipelineEvent::SessionEnded);
    }

    // ── Session machine plumbing ─────────────────────────────────────────

    fn dispatch(&mut self, input: Input) {
        let outputs = self.session.handle(input);
        self.act_all(&outputs);
    }

    fn act_all(&mut self, outputs: &[Output]) {
        for output in outputs {
            self.act(output);
        }
    }

    fn act(&mut self, output: &Output) {
        let actions = route_output(output, &mut self.playback);

        if actions.start_capture {
            self.capturing = true;
        }
        if actions.stop_capture {
            self.capturing = false;
        }
        if let Some(text) = actions.show_partial {
            let _ = self.events_tx.send(PipelineEvent::Partial(text));
        }
        if let Some((turn, text)) = actions.submit_utterance {
            self.agent.submit(turn, &text);
            // This demo-grade wiring treats submission as always
            // succeeding immediately; a real backend would report
            // SubmitSucceeded/SubmitFailed asynchronously instead.
            self.dispatch(Input::SubmitSucceeded { turn });
        }
        if let Some(turn) = actions.cancel_synthesis {
            self.tts.cancel(turn);
        }
        if let Some(turn) = actions.replay_preroll {
            self.replay_preroll_into_stt(turn);
        }
        if actions.resumed_turn.is_some() {
            self.bargein.notify_playback_started();
            self.bargein.reset();
        }
        if let Some(state) = actions.state_changed {
            self.on_state_changed(state);
        }
    }

    fn on_state_changed(&mut self, state: SessionState) {
        let _ = self.events_tx.send(PipelineEvent::StateChanged(state));
        match state {
            SessionState::Listening => {
                self.endpoint.reset();
                self.stt_active_turn = None;
                if self.metrics.has_any() {
                    let _ = self
                        .events_tx
                        .send(PipelineEvent::TurnMetrics(self.metrics.summary()));
                }
                self.metrics = TurnMetrics::new();
            }
            SessionState::UserSpeaking => {
                // A turn also ends by being interrupted, not only by returning
                // to Listening. Flushing metrics solely on the Listening
                // transition loses them for exactly the turns we most want to
                // measure: in continuous conversation the machine goes
                // Interrupting -> UserSpeaking directly, so every barge-in
                // latency — the number this product is judged on — was
                // discarded at the moment it finally became available.
                if self.metrics.has_any() {
                    let _ = self
                        .events_tx
                        .send(PipelineEvent::TurnMetrics(self.metrics.summary()));
                    self.metrics = TurnMetrics::new();
                }
                if self.stt_active_turn.is_none() {
                    if let Some(turn) = self.session.current_turn() {
                        self.resampler_16k.reset();
                        self.stt_active_turn = Some(turn);
                    }
                }
            }
            SessionState::Finalizing => {
                self.metrics.mark_endpoint();
                if let Some(turn) = self.session.current_turn() {
                    self.stt.finish_utterance(turn);
                }
            }
            SessionState::Interrupting => {
                self.bargein.notify_playback_stopped();
            }
            SessionState::Idle => {
                self.endpoint.reset();
                self.bargein.reset();
                self.stt.reset();
                self.stt_active_turn = None;
            }
            SessionState::Submitting
            | SessionState::AwaitingAgent
            | SessionState::AgentSpeaking => {}
        }
    }

    fn replay_preroll_into_stt(&mut self, turn: TurnId) {
        self.resampler_16k.reset();
        let frames = self.session.preroll_frames();
        let mut samples_16k = Vec::new();
        for frame in &frames {
            samples_16k.extend(self.resampler_16k.process(frame));
        }
        if !samples_16k.is_empty() {
            self.stt.push_preroll_audio(turn, samples_16k);
        }
        self.stt_active_turn = Some(turn);
    }

    // ── STT ───────────────────────────────────────────────────────────────

    fn handle_stt_result(&mut self, result: SttResult) {
        match result {
            SttResult::Partial { turn, text } => {
                self.dispatch(Input::PartialTranscript { turn, text });
            }
            SttResult::Final { turn, text } => {
                if self.session.is_current(turn) {
                    self.metrics.mark_final_transcript();
                }
                let _ = self
                    .events_tx
                    .send(PipelineEvent::FinalTranscript(text.clone()));
                self.dispatch(Input::FinalTranscript { turn, text });
            }
        }
    }

    // ── Agent backend ────────────────────────────────────────────────────

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextChunk { turn, text } => {
                let already_started = self.playback.recognises(turn);
                let recognized = already_started || self.session.current_turn() == Some(turn);
                if !recognized {
                    return; // stale: the turn was already abandoned or never started.
                }
                if !already_started {
                    // First chunk seen for this turn: playback starts here.
                    self.playback.start(turn);
                    self.bargein.notify_playback_started();
                    self.bargein.reset();
                }
                let _ = self.events_tx.send(PipelineEvent::AgentText(text.clone()));
                // The session machine tracks only the control-flow
                // consequence of this (AwaitingAgent -> AgentSpeaking on
                // the first chunk); it emits no Output for it, so there's
                // nothing further to `act` on here.
                let _ = self.session.handle(Input::AgentTextArrived {
                    turn,
                    text: text.clone(),
                });

                for phrase in buzztalk_tts::segment_into_phrases(&text) {
                    self.tts.synthesize(turn, phrase);
                }
            }
            AgentEvent::TurnComplete { turn } => {
                self.dispatch(Input::AgentTurnComplete { turn });
            }
        }
    }

    // ── Audio pump ────────────────────────────────────────────────────────

    fn pump_audio(&mut self) {
        if let Some(sim) = &self.simulate_frames {
            while let Some(frame) = self.engine.try_recv_render_ref() {
                self.pending_ref.push_back(frame);
            }
            let started = *self.sim_start.get_or_insert_with(Instant::now);
            let elapsed_frames =
                (started.elapsed().as_millis() / buzztalk_core::FRAME_MS as u128) as usize;
            let target = elapsed_frames.min(sim.len());
            while self.sim_cursor < target {
                self.pending_cap.push_back(sim[self.sim_cursor].clone());
                self.sim_cursor += 1;
            }
        } else {
            while let Some(frame) = self.engine.try_recv_render_ref() {
                self.pending_ref.push_back(frame);
            }
            while let Some(frame) = self.engine.try_recv_capture() {
                self.pending_cap.push_back(frame);
            }
        }

        while !self.pending_ref.is_empty() && !self.pending_cap.is_empty() {
            let far = self.pending_ref.pop_front().unwrap();
            let mut near = self.pending_cap.pop_front().unwrap();
            self.process_frame(&far, &mut near);
        }
    }

    fn process_frame(&mut self, far: &[f32], near: &mut [f32]) {
        let _ = self.aec.process_render(far);
        let _ = self.aec.process_capture(near);

        self.bargein.set_aec_stats(self.aec.stats());
        let route = self.forced_output_route.unwrap_or_else(detect_output_route);
        self.bargein.set_output_route(route);

        let Ok(frame): Result<Frame, _> = (&*near).try_into() else {
            return; // Invariant violation: the engine always yields FRAME_SAMPLES.
        };

        if self.capturing {
            self.session.push_frame(frame);
        }

        match self.session.state() {
            SessionState::AgentSpeaking => {
                if let Ok(DetectorEvent::SpeechStart) = self.bargein.push_frame(near) {
                    self.metrics.mark_barge_in();
                    let outputs = self.session.handle(Input::BargeInConfirmed);
                    let armed = outputs.iter().any(|o| matches!(o, Output::CancelPlayback));
                    self.act_all(&outputs);
                    if armed {
                        self.silence_watch = Some(self.engine.stats().playback_underrun_samples);
                    }
                }
            }
            SessionState::Listening | SessionState::UserSpeaking => {
                if let Ok(event) = self.endpoint.push_frame(near) {
                    if !matches!(event, DetectorEvent::Idle) {
                        self.dispatch(Input::EndpointEvent(event));
                    }
                }
            }
            _ => {}
        }

        if let Some(turn) = self.stt_active_turn {
            if matches!(
                self.session.state(),
                SessionState::UserSpeaking | SessionState::Interrupting
            ) {
                let samples_16k = self.resampler_16k.process(near);
                if !samples_16k.is_empty() && !self.stt.push_audio(turn, samples_16k) {
                    let _ = self.events_tx.send(PipelineEvent::Dropped {
                        what: "STT audio (recognizer fell behind)",
                        total: self.stt.dropped_count(),
                    });
                }
            }
        }
    }

    // ── Playback feeder ──────────────────────────────────────────────────

    /// Top the engine's playback ring up to a small, bounded lead
    /// ([`JIT_LEAD_FRAMES`]) rather than handing over a whole utterance at
    /// once. See [`crate::playback`]'s module docs for why: this is what
    /// makes `Output::CancelPlayback` able to actually silence the speaker
    /// within tens of milliseconds, given `buzztalk-audio` exposes no way
    /// to discard samples already queued.
    fn feed_playback(&mut self) {
        let lead_samples = JIT_LEAD_FRAMES * FRAME_SAMPLES;

        if self.playback.live_len() == 0 && self.playback_pushed_samples == 0 {
            return;
        }

        let now = Instant::now();
        let anchor = *self.playback_push_started_at.get_or_insert(now);
        let elapsed_samples = (now.duration_since(anchor).as_secs_f64()
            * buzztalk_core::SAMPLE_RATE_HZ as f64) as u64;
        let resident = self.playback_pushed_samples.saturating_sub(elapsed_samples);

        if (resident as usize) < lead_samples {
            let want = lead_samples - resident as usize;
            let chunk = self.playback.take_live(want);
            if !chunk.is_empty() {
                let dropped = self.engine.push_playback(&chunk);
                self.playback_pushed_samples += chunk.len() as u64;
                if dropped > 0 {
                    let _ = self.events_tx.send(PipelineEvent::Dropped {
                        what: "playback ring overrun",
                        total: dropped as u64,
                    });
                }
            }
        }

        if self.playback.live_len() == 0 && resident == 0 {
            self.playback_pushed_samples = 0;
            self.playback_push_started_at = None;
        }
    }

    fn check_silence_watch(&mut self) {
        if let Some(baseline) = self.silence_watch {
            let current = self.engine.stats().playback_underrun_samples;
            if current > baseline {
                self.metrics.mark_playback_silent();
                self.silence_watch = None;
            }
        }
    }

    /// Surface `PlaybackState`'s stale/late-TTS-result drop counter as a
    /// `PipelineEvent::Dropped` whenever it moves.
    fn report_stale_chunks(&mut self) {
        let total = self.playback.stale_chunks_dropped();
        if total > self.stale_chunks_reported {
            self.stale_chunks_reported = total;
            let _ = self.events_tx.send(PipelineEvent::Dropped {
                what: "synthesized audio for an abandoned/unrecognised turn",
                total,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    //! Both tests below require the real Parakeet and Pocket TTS models on
    //! disk and are `#[ignore]`d for CI. They exist to pin down a real
    //! finding: `buzztalk-stt` (sherpa-onnx, statically linked onnxruntime)
    //! and `buzztalk-tts` (the `ort` crate, dynamically loaded onnxruntime)
    //! each load and run correctly in isolation -- see `buzztalk-stt` and
    //! `buzztalk-tts`'s own `--ignored` test suites -- but as soon as
    //! *both* are constructed in the same process, onnxruntime's C API
    //! resolves to the wrong version and the process segfaults:
    //!
    //! ```text
    //! The requested API version [27] is not available, only API versions
    //! [1, 24] are supported in this build. Current ORT Version is: 1.24.2
    //! ```
    //! ```text
    //! process didn't exit successfully (signal: 11, SIGSEGV: invalid
    //! memory reference)
    //! ```
    //!
    //! This reproduces regardless of construction order (both orderings
    //! are exercised below), which points at a link-time symbol collision
    //! between sherpa-onnx-sys's statically-linked onnxruntime and `ort`'s
    //! separately-loaded onnxruntime, not a call-order race. `buzztalk-pipeline`
    //! is the first crate in this workspace to link both native
    //! dependencies into one binary, which is why this was never visible
    //! before. See this crate's implementation report for the full writeup;
    //! not something owned or fixable from this crate.

    #[test]
    #[ignore = "requires the real Parakeet + Pocket TTS models on disk; segfaults today, see module docs"]
    fn stt_then_tts_construct_together() {
        let _stt = crate::stt_worker::SttWorker::spawn().expect("stt should load");
        eprintln!("STT loaded OK");
        let _tts = crate::tts_worker::TtsWorker::spawn().expect("tts should load");
        eprintln!("TTS loaded OK");
    }

    #[test]
    #[ignore = "requires the real Parakeet + Pocket TTS models on disk; segfaults today, see module docs"]
    fn tts_then_stt_construct_together() {
        let _tts = crate::tts_worker::TtsWorker::spawn().expect("tts should load");
        eprintln!("TTS loaded OK");
        let _stt = crate::stt_worker::SttWorker::spawn().expect("stt should load");
        eprintln!("STT loaded OK");
    }
}
