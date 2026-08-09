//! [`ConversationPipeline`]: owns the orchestration thread plus the STT and
//! TTS worker threads, and wires everything sibling crates provide into an
//! actual, interruptible conversation. See the crate-level docs for the
//! architecture and the design rationale behind the trickier parts
//! ([`crate::playback`] for cancellation, [`crate::agent`] for the test
//! backend).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use buzztalk_aec::{new_best_available, NullCanceller};
use buzztalk_audio::{detect_output_route, DuplexConfig, DuplexEngine};
use buzztalk_core::{DetectorEvent, EchoCanceller, OutputRoute, SpeechDetector, FRAME_SAMPLES};
use buzztalk_session::{Frame, Input, Output, SessionMachine, SessionState, TurnId};
use buzztalk_stt::Resampler48to16;
use buzztalk_vad::{BargeInDetector, EndpointConfig, EndpointDetector};

use crate::agent::{AgentBackend, AgentEvent, EchoAgent};
use crate::audio_engine::AudioEngine;
use crate::capture;
use crate::error::PipelineError;
use crate::metrics::TurnMetrics;
use crate::playback::{route_output, PlaybackState, JIT_LEAD_FRAMES};
use crate::stt_worker::WorkerPoll as SttPoll;
use crate::stt_worker::{SttResult, SttWorker};
use crate::tts_worker::TtsWorker;
use crate::tts_worker::WorkerPoll as TtsPoll;

/// How often the orchestration loop wakes up to check for new capture
/// frames, worker results, and control messages. Short enough to keep the
/// playback JIT lead ([`JIT_LEAD_FRAMES`]) topped up smoothly; matches the
/// cadence `buzztalk-labs`' harnesses already use for the same reason.
const LOOP_SLEEP: Duration = Duration::from_millis(5);

/// How long capture may go without producing a single frame before the
/// orchestrator gives up waiting and reports the microphone as lost.
///
/// `buzztalk-audio`'s `DuplexEngine` has no device-disconnect signal of its
/// own (its cpal error callback only logs -- see this crate's
/// implementation report for why that's a gap worth closing upstream, not
/// something this crate can fix from here). This is the best available
/// substitute: real capture delivers a frame every [`buzztalk_core::FRAME_MS`]
/// (10 ms) when a device is present, so anything past a couple of orders of
/// magnitude beyond that is not jitter, it's silence with nothing behind
/// it. Generous enough to never fire on a healthy device under load.
const MIC_STALL_TIMEOUT: Duration = Duration::from_millis(1500);

/// How often the orchestrator polls the host's default-device signature
/// and the engine's failure flag. A couple of CoreAudio property reads per
/// second -- negligible next to the audio work, fast enough that a device
/// swap heals within about a second.
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_millis(1000);

/// How many consecutive real speech turns may finalize with no words
/// before the dead-capture watchdog rebuilds the engine. Two would risk
/// firing on an unlucky pair of coughs; three is a strong signal the
/// capture stream itself has gone bad (see `handle_stt_result`).
const DEAD_CAPTURE_EMPTY_FINALS: u32 = 3;

/// The recipe for reopening the audio engine on the current default
/// devices, kept by the orchestrator so it can rebuild live when a device
/// disappears or renegotiates its format. `None` in simulate mode.
type EngineFactory = Box<dyn Fn() -> buzztalk_core::Result<Box<dyn AudioEngine>> + Send>;

/// Open the audio engine `PipelineConfig` asked for: `VoiceProcessingIO`
/// when `use_voice_processing` is set (macOS only -- see
/// [`PipelineConfig::use_voice_processing`]'s docs for the non-macOS
/// behaviour), `DuplexEngine` otherwise. Shared by [`ConversationPipeline::start`]
/// and the [`EngineFactory`] closure the device watchdog rebuilds through,
/// so a live rebuild can never silently switch engines mid-session.
fn open_engine(
    duplex: DuplexConfig,
    use_voice_processing: bool,
) -> buzztalk_core::Result<Box<dyn AudioEngine>> {
    if use_voice_processing {
        #[cfg(target_os = "macos")]
        {
            return buzztalk_audio::VoiceProcessingEngine::start(duplex)
                .map(|e| Box::new(e) as Box<dyn AudioEngine>);
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(buzztalk_core::Error::Device(
                "PipelineConfig::use_voice_processing requires macOS -- VoiceProcessingIO is a \
                 macOS-only CoreAudio unit; use the default DuplexEngine path on this platform"
                    .to_string(),
            ));
        }
    }
    DuplexEngine::start(duplex).map(|e| Box::new(e) as Box<dyn AudioEngine>)
}

/// Which halves of [`buzztalk_audio::default_devices_signature`] the device
/// watchdog should react to -- a direction pinned to an explicit device in
/// [`DuplexConfig`] ignores changes to the host default for that direction.
#[derive(Clone, Copy)]
struct SignatureMask {
    input: bool,
    output: bool,
}

impl SignatureMask {
    /// Reduce a full `in:...|out:...` signature to only the tracked parts.
    fn apply(&self, full: &str) -> String {
        full.split('|')
            .filter(|part| {
                (self.input && part.starts_with("in:"))
                    || (self.output && part.starts_with("out:"))
            })
            .collect::<Vec<_>>()
            .join("|")
    }
}

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
    /// Run with [`buzztalk_aec::NullCanceller`] instead of the best
    /// compiled-in AEC backend -- i.e. no echo cancellation at all. Exists
    /// so a user whose barge-in gate never opens can tell whether AEC
    /// itself is the culprit: with this set, the ERLE gate that normally
    /// requires evidence of real echo suppression before trusting a
    /// candidate barge-in is moot (there is no suppression to measure), so
    /// barge-in becomes far more trigger-happy on ordinary playback bleed.
    /// See [`ConversationPipeline::start`]'s caller (`buzztalk-demo`'s
    /// `--no-aec`) for the loud warning this is meant to come with.
    pub no_aec: bool,
    /// End-of-speech silence, in milliseconds, before the endpointer
    /// declares the user's turn over. `None` keeps
    /// [`buzztalk_vad::EndpointConfig`]'s default (~300 ms). Live testing
    /// (2026-08-09) showed 300 ms splits thinking-aloud sentences into
    /// separate turns; ~1000 ms suits conversational speech.
    pub endpoint_silence_ms: Option<u64>,
    /// Use macOS's `VoiceProcessingIO` audio unit instead of `DuplexEngine`'s
    /// two independent `cpal` streams. Opening separate `cpal` input and
    /// output streams on a Bluetooth headset starves its microphone to
    /// digital silence -- two independent CoreAudio clients each grabbing
    /// a half of the same Bluetooth device don't coordinate the handset's
    /// HFP profile the way asking for both directions from one client does
    /// (measured; see `docs/live-session-2026-08-09/SESSION-REPORT.md`
    /// "duplex-on-BT"). `VoiceProcessingIO` runs capture and render as one
    /// audio session and handles Bluetooth correctly.
    ///
    /// **macOS only.** On every other platform, setting this `true`
    /// doesn't fail to compile -- it fails [`ConversationPipeline::start`]
    /// with a clear [`PipelineError`] instead, so a cross-platform caller
    /// can gate this behind a flag without needing its own `cfg`.
    /// Defaults to `false` (the existing `DuplexEngine` path). Ignored
    /// when [`Self::simulate_capture`] is set: simulate mode replaces
    /// capture with WAV-sourced frames regardless of which engine renders
    /// playback, and `DuplexEngine` is the better-exercised path for that.
    pub use_voice_processing: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            duplex: DuplexConfig::default(),
            forced_output_route: None,
            simulate_capture: None,
            agent: Box::new(EchoAgent::new()),
            no_aec: false,
            endpoint_silence_ms: None,
            use_voice_processing: false,
        }
    }
}

/// What the pipeline can actually do this session, decided once at startup
/// and never revised upward -- see [`PipelineEvent::CapabilityLost`] for
/// what happens when a capability present at startup is lost later.
///
/// The crate's design rule: *every failure degrades to something usable.
/// Losing echo cancellation must not lose voice; losing voice must not lose
/// Buzz.* Neither STT nor TTS failing to load prevents
/// [`ConversationPipeline::start`] from succeeding -- this is how the
/// caller learns what it actually got.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    /// Speech-to-text is available: the user's voice will be transcribed,
    /// and utterances can be submitted to the agent.
    pub stt: bool,
    /// Text-to-speech is available: the agent's replies will be spoken.
    pub tts: bool,
    /// Why STT is unavailable, if it is (e.g. a missing model directory).
    pub stt_unavailable_reason: Option<String>,
    /// Why TTS is unavailable, if it is.
    pub tts_unavailable_reason: Option<String>,
}

impl Capabilities {
    /// A plain-language, one-paragraph report of what the user actually
    /// has this session -- suitable for printing at startup so a missing
    /// model shows up as "degraded but working," never as silence.
    pub fn report(&self) -> String {
        match (self.stt, self.tts) {
            (true, true) => {
                "full duplex: your voice will be transcribed and the agent's replies will be spoken"
                    .to_string()
            }
            (true, false) => format!(
                "degraded: speech-to-text only -- your voice will be transcribed and \
                 submitted normally, but the agent's replies will arrive as text only, \
                 not speech (text-to-speech unavailable{})",
                reason_suffix(&self.tts_unavailable_reason)
            ),
            (false, true) => format!(
                "degraded: text-to-speech only -- the agent's replies will be spoken, \
                 but your voice cannot be transcribed or submitted \
                 (speech-to-text unavailable{})",
                reason_suffix(&self.stt_unavailable_reason)
            ),
            (false, false) => format!(
                "degraded: neither speech-to-text nor text-to-speech is available \
                 (speech-to-text unavailable{}; text-to-speech unavailable{}) -- \
                 the pipeline is still running and will still report state, it just \
                 has nothing to transcribe or speak with until both are fixed",
                reason_suffix(&self.stt_unavailable_reason),
                reason_suffix(&self.tts_unavailable_reason),
            ),
        }
    }
}

fn reason_suffix(reason: &Option<String>) -> String {
    match reason {
        Some(r) => format!(": {r}"),
        None => String::new(),
    }
}

/// Events the orchestration thread reports outward -- e.g. to a terminal
/// demo or a UI.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// What the pipeline can actually do, reported exactly once, before any
    /// other event, right after the orchestration thread starts. Always
    /// sent, even when both engines failed to load -- see [`Capabilities`].
    Capabilities(Capabilities),
    /// A capability that was available (whether at startup or after
    /// recovering) has been lost mid-session -- an engine's worker thread
    /// died, or the microphone stopped delivering audio. The rest of the
    /// pipeline keeps running; this is purely informational so the host can
    /// tell the user.
    CapabilityLost {
        /// What was lost, e.g. `"speech-to-text"`, `"text-to-speech"`, or
        /// `"microphone"`.
        what: &'static str,
        /// Why, in human-readable terms.
        reason: String,
    },
    /// The audio engine was torn down and rebuilt on the current default
    /// devices — a stream errored, the default device or its sample rate
    /// changed (Bluetooth headsets renegotiate around profile switches and
    /// silently corrupt a stale-rate stream), or capture stalled. The
    /// session was restarted; anything mid-utterance was abandoned.
    AudioDeviceRebuilt {
        /// Which trigger fired, in human-readable terms.
        reason: String,
    },
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
    capabilities: Capabilities,
}

impl ConversationPipeline {
    /// Build every component that can fail, then spawn the orchestration
    /// thread.
    ///
    /// Only the audio engine and (for `--simulate`) the WAV fixture are
    /// fatal to construct: without a working audio device (or its
    /// simulated stand-in) there is nothing at all for this pipeline to do.
    /// STT and TTS are each best-effort -- per the crate's design rule
    /// ("every failure degrades to something usable"), a missing or
    /// unloadable model for either one does not fail `start`; it's recorded
    /// in the returned pipeline's [`Capabilities`] (also available via
    /// [`Self::capabilities`] and as the very first [`PipelineEvent`]) and
    /// the pipeline runs with whatever it actually has.
    pub fn start(config: PipelineConfig) -> Result<Self, PipelineError> {
        let PipelineConfig {
            duplex,
            forced_output_route,
            simulate_capture,
            agent,
            no_aec,
            endpoint_silence_ms,
            use_voice_processing,
        } = config;

        // Simulate mode replaces capture with WAV-sourced frames regardless
        // of which engine renders playback -- `DuplexEngine` is the
        // better-exercised path for that, so VPIO is only actually used
        // when both are requested. See `PipelineConfig::use_voice_processing`'s
        // docs.
        let use_voice_processing = use_voice_processing && simulate_capture.is_none();

        let engine: Box<dyn AudioEngine> = open_engine(duplex.clone(), use_voice_processing)?;
        // Real devices come and go (and Bluetooth headsets renegotiate
        // formats under a running stream) -- keep the recipe for opening
        // the engine so the orchestrator can rebuild it live. Simulate
        // mode reads a WAV; there is nothing to rebuild.
        // When the config pins an explicit device for a direction, changes
        // to the host's *default* for that direction are irrelevant -- the
        // engine isn't following the default there. macOS flips defaults
        // freely around Bluetooth profile switches, and tracking a pinned
        // direction turns every flip into a needless rebuild.
        let signature_mask = SignatureMask {
            input: duplex.input_device.is_none(),
            output: duplex.output_device.is_none(),
        };
        let engine_factory: Option<EngineFactory> = simulate_capture.is_none().then(|| {
            let cfg = duplex;
            Box::new(move || open_engine(cfg.clone(), use_voice_processing)) as EngineFactory
        });
        let device_signature = simulate_capture
            .is_none()
            .then(|| signature_mask.apply(&buzztalk_audio::default_devices_signature()));
        let aec: Box<dyn EchoCanceller> = if no_aec {
            Box::new(NullCanceller)
        } else {
            new_best_available()
        };
        let simulate_frames = match &simulate_capture {
            Some(path) => Some(capture::load_as_capture_frames(path)?),
            None => None,
        };

        let (stt, stt_unavailable_reason) = match SttWorker::spawn() {
            Ok(worker) => (Some(worker), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let (tts, tts_unavailable_reason) = match TtsWorker::spawn() {
            Ok(worker) => (Some(worker), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let capabilities = Capabilities {
            stt: stt.is_some(),
            tts: tts.is_some(),
            stt_unavailable_reason,
            tts_unavailable_reason,
        };

        let (control_tx, control_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();

        let endpoint = match endpoint_silence_ms {
            Some(ms) => {
                let frame_ms = u64::from(buzztalk_core::FRAME_MS);
                EndpointDetector::new(EndpointConfig {
                    // Round up so e.g. 995 ms never silently shortens the
                    // requested window, and never drop below one frame.
                    hangover_frames: (ms.div_ceil(frame_ms)).max(1) as u32,
                    ..EndpointConfig::default()
                })
            }
            None => EndpointDetector::default(),
        };
        let mut orchestrator = Orchestrator {
            engine,
            engine_factory,
            device_signature,
            signature_mask,
            device_check_elapsed: Duration::ZERO,
            rebuild_failures: 0,
            stall_rebuild_attempted: false,
            consecutive_empty_finals: 0,
            no_aec,
            aec,
            endpoint,
            bargein: BargeInDetector::new(Default::default()),
            session: SessionMachine::new(),
            resampler_16k: Resampler48to16::new(),
            stt,
            tts,
            agent,
            playback: PlaybackState::new(),
            capabilities: capabilities.clone(),
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
            mic_silence_elapsed: Duration::ZERO,
            mic_stall_reported: false,
            saw_capture_frame_this_tick: false,
            tts_pending: HashMap::new(),
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
            capabilities,
        })
    }

    /// What this pipeline can actually do this session -- the same value
    /// reported as the first [`PipelineEvent::Capabilities`], available
    /// synchronously so a caller can print it before pumping events at all.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
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
    engine: Box<dyn AudioEngine>,
    /// See [`EngineFactory`]. `None` in simulate mode and in tests, which
    /// also disables the device watchdog entirely.
    engine_factory: Option<EngineFactory>,
    /// The default-device fingerprint the current engine was opened
    /// against ([`buzztalk_audio::default_devices_signature`], reduced by
    /// [`SignatureMask`]); a change means the engine's streams are stale
    /// even if no error fired.
    device_signature: Option<String>,
    /// See [`SignatureMask`].
    signature_mask: SignatureMask,
    /// Accumulates toward [`DEVICE_CHECK_INTERVAL`].
    device_check_elapsed: Duration,
    /// Consecutive failed rebuild attempts, for throttling the error spam
    /// while a device is genuinely gone.
    rebuild_failures: u32,
    /// Whether the current mic stall has already spent its one rebuild
    /// attempt -- a stall that persists across a rebuild means the device
    /// is silent, not stale, and rebuilding again won't help. Cleared when
    /// capture frames flow again or the device signature changes.
    stall_rebuild_attempted: bool,
    /// Consecutive real speech turns that finalized with no words -- the
    /// dead-capture signal (see [`DEAD_CAPTURE_EMPTY_FINALS`] and
    /// `handle_stt_result`). Reset by any word-bearing final or a rebuild.
    consecutive_empty_finals: u32,
    /// Recreate the canceller to match a rebuilt engine (`--no-aec` state).
    no_aec: bool,
    aec: Box<dyn EchoCanceller>,
    endpoint: EndpointDetector,
    bargein: BargeInDetector,
    session: SessionMachine,
    resampler_16k: Resampler48to16,
    /// `None` if the recognizer never loaded, or once its worker thread has
    /// died mid-session -- see [`Orchestrator::drain_stt_results`]. Every
    /// use site treats this as "no STT this session," not an error.
    stt: Option<SttWorker>,
    /// `None` if the synthesizer never loaded, or once its worker thread
    /// has died mid-session -- see [`Orchestrator::drain_tts_results`].
    tts: Option<TtsWorker>,
    agent: Box<dyn AgentBackend>,
    playback: PlaybackState,
    /// What was true at startup; kept only to compute the diff when a
    /// capability is lost mid-session (`stt`/`tts` going from `Some` to
    /// `None` is the live truth, this is just the "what changed" baseline).
    capabilities: Capabilities,

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

    /// How long it's been since a capture frame last arrived from the real
    /// device (always zero in `--simulate` mode, which paces its own
    /// frames and is never treated as a device that can "disappear"). Reset
    /// to zero every time a capture frame actually arrives; once it crosses
    /// [`MIC_STALL_TIMEOUT`] the microphone is reported lost.
    mic_silence_elapsed: Duration,
    /// Whether the current stall has already been reported, so
    /// [`PipelineEvent::CapabilityLost`] fires once per stall, not once per
    /// tick for as long as the mic stays gone.
    mic_stall_reported: bool,
    /// Set by [`Self::pump_audio`] each tick, read (and reset) by
    /// [`Self::check_mic_health`] right after -- whether at least one real
    /// capture frame arrived this tick.
    saw_capture_frame_this_tick: bool,

    /// Synthesis requests sent to the TTS worker for a turn that haven't
    /// yet produced a result (regardless of whether that result ends up
    /// played or dropped as stale). Consulted before declaring a turn's
    /// playback fully drained -- see [`Self::nothing_left_to_play`] -- so
    /// the orchestrator never ends a turn while its last phrase is still
    /// being synthesized.
    tts_pending: HashMap<TurnId, u32>,

    events_tx: Sender<PipelineEvent>,
    control_rx: Receiver<PipelineControl>,
    running: bool,
}

impl Orchestrator {
    fn run(&mut self) {
        self.emit_capabilities();

        let mut last_tick = Instant::now();
        while self.running {
            self.drain_control();
            if !self.running {
                break;
            }

            let now = Instant::now();
            let dt = now.duration_since(last_tick);
            last_tick = now;
            self.tick(dt);

            thread::sleep(LOOP_SLEEP);
        }

        self.dispatch(Input::SessionEnd);
        let _ = self.events_tx.send(PipelineEvent::SessionEnded);
    }

    /// Report [`Capabilities`] exactly once, before anything else -- always
    /// sent even when both engines are missing, so a caller polling events
    /// never mistakes silence for "still starting up."
    fn emit_capabilities(&self) {
        let _ = self
            .events_tx
            .send(PipelineEvent::Capabilities(self.capabilities.clone()));
    }

    /// Drain every pending control message. Split out from [`Self::run`] so
    /// a test can push control messages onto `control_rx` and then call
    /// this directly, without spawning the orchestration thread or waiting
    /// on real time.
    fn drain_control(&mut self) {
        while let Ok(ctrl) = self.control_rx.try_recv() {
            match ctrl {
                PipelineControl::StartSession => self.dispatch(Input::SessionStart),
                PipelineControl::EndSession => self.dispatch(Input::SessionEnd),
                PipelineControl::PushToTalkPressed => self.dispatch(Input::PushToTalkPressed),
                PipelineControl::PushToTalkReleased => self.dispatch(Input::PushToTalkReleased),
                PipelineControl::Shutdown => self.running = false,
            }
        }
    }

    /// One pass of everything the orchestration loop does per wake-up,
    /// given how much time has passed (`dt`). Split out from [`Self::run`]
    /// so tests can drive the whole conversation loop deterministically --
    /// with a fake audio engine and fake STT/TTS workers, no real sleeping,
    /// and a `dt` the test controls -- instead of only through a spawned
    /// thread racing real wall-clock time.
    fn tick(&mut self, dt: Duration) {
        self.dispatch(Input::Tick(dt));

        self.pump_audio();
        self.check_mic_health(dt);
        self.check_device_health(dt);

        self.drain_stt_results();

        while let Some(event) = self.agent.poll() {
            self.handle_agent_event(event);
        }

        self.drain_tts_results();
        self.report_stale_chunks();

        self.feed_playback();
        self.check_silence_watch();
    }

    /// Pop every available STT result, tolerating an absent or newly-dead
    /// recognizer -- see the `stt` field's docs.
    fn drain_stt_results(&mut self) {
        loop {
            let Some(stt) = &self.stt else { return };
            match stt.poll() {
                SttPoll::Empty => return,
                SttPoll::Result(result) => self.handle_stt_result(result),
                SttPoll::Disconnected => self.lose_capability(
                    "speech-to-text",
                    "recognizer worker thread ended unexpectedly",
                    |orch| orch.stt = None,
                ),
            }
        }
    }

    /// Pop every available TTS result, tolerating an absent or newly-dead
    /// synthesizer -- see the `tts` field's docs.
    fn drain_tts_results(&mut self) {
        loop {
            let Some(tts) = &self.tts else { return };
            match tts.poll() {
                TtsPoll::Empty => return,
                TtsPoll::Result(result) => {
                    if let Some(pending) = self.tts_pending.get_mut(&result.turn) {
                        *pending = pending.saturating_sub(1);
                    }
                    self.playback
                        .route_synthesized(result.turn, &result.samples_48k);
                }
                TtsPoll::Disconnected => self.lose_capability(
                    "text-to-speech",
                    "synthesizer worker thread ended unexpectedly",
                    |orch| {
                        orch.tts = None;
                        // Nothing still "in flight" will ever produce a
                        // result now -- clear every pending count so
                        // `nothing_left_to_play` doesn't wait forever on
                        // synthesis that can no longer happen.
                        orch.tts_pending.clear();
                    },
                ),
            }
        }
    }

    /// Common "a capability just disappeared" bookkeeping: apply `clear`
    /// (which sets the relevant field to `None`), then report it exactly
    /// once via [`PipelineEvent::CapabilityLost`]. Idempotent to call
    /// repeatedly -- `clear` running again on an already-`None` field is
    /// harmless -- but callers only reach this from the `Some` arm of a
    /// poll, so in practice it fires once per real loss.
    fn lose_capability(&mut self, what: &'static str, reason: &str, clear: impl FnOnce(&mut Self)) {
        clear(self);
        let _ = self.events_tx.send(PipelineEvent::CapabilityLost {
            what,
            reason: reason.to_string(),
        });
    }

    /// Track how long it's been since a capture frame last arrived from a
    /// real (non-`--simulate`) device, and report the microphone as lost
    /// once that crosses [`MIC_STALL_TIMEOUT`]. See the `mic_silence_elapsed`
    /// field's docs for why this heuristic exists instead of a real
    /// disconnect signal.
    fn check_mic_health(&mut self, dt: Duration) {
        if self.simulate_frames.is_some() {
            return; // Paced by the WAV, not a device that can "unplug".
        }
        if self.saw_capture_frame_this_tick {
            self.mic_silence_elapsed = Duration::ZERO;
            self.mic_stall_reported = false;
            return;
        }
        self.mic_silence_elapsed += dt;
        if self.mic_silence_elapsed >= MIC_STALL_TIMEOUT && !self.mic_stall_reported {
            self.mic_stall_reported = true;
            let _ = self.events_tx.send(PipelineEvent::CapabilityLost {
                what: "microphone",
                reason: format!(
                    "no capture audio received for over {:?} (device disconnected?)",
                    MIC_STALL_TIMEOUT
                ),
            });
        }
    }

    /// Watchdog for the real audio device: every
    /// [`DEVICE_CHECK_INTERVAL`], rebuild the engine when a stream has
    /// errored, the host's default-device signature has changed (the
    /// silent Bluetooth renegotiation case -- the stale stream keeps
    /// running but feeds the recognizer time-warped audio), or capture has
    /// stalled (one attempt per stall). Live testing 2026-08-09: every
    /// manual daemon restart instantly fixed transcription on an untouched
    /// headset -- this is that restart, automated and scoped to the
    /// engine.
    fn check_device_health(&mut self, dt: Duration) {
        // `rebuild_engine` re-fetches the factory; this is just the guard
        // that there is one (and we're not in simulate mode).
        if self.simulate_frames.is_some() || self.engine_factory.is_none() {
            return;
        }
        self.device_check_elapsed += dt;
        if self.device_check_elapsed < DEVICE_CHECK_INTERVAL {
            return;
        }
        self.device_check_elapsed = Duration::ZERO;

        let signature_now = self
            .signature_mask
            .apply(&buzztalk_audio::default_devices_signature());
        let signature_changed = self
            .device_signature
            .as_deref()
            .is_some_and(|s| s != signature_now);
        let failed = self.engine.failed();
        let stalled = self.mic_stall_reported && !self.stall_rebuild_attempted;
        if self.saw_capture_frame_this_tick || !self.mic_stall_reported {
            // Frames are flowing (or never stalled): a future stall gets a
            // fresh rebuild attempt.
            self.stall_rebuild_attempted = false;
        }
        if !(failed || signature_changed || stalled) {
            return;
        }

        let reason = if signature_changed {
            format!("default device changed ({signature_now})")
        } else if failed {
            "audio stream error".to_string()
        } else {
            "capture stalled".to_string()
        };
        if stalled && !failed && !signature_changed {
            self.stall_rebuild_attempted = true;
        }

        self.rebuild_engine(reason, Some(signature_now));
    }

    /// Tear down the current engine and open a fresh one on the current
    /// default devices, resetting every piece of state that was tied to the
    /// old streams (canceller adaptation, resampler phase, detector noise
    /// floors, half-fed utterance) and restarting the session. Shared by
    /// the device watchdog and the dead-capture watchdog. `new_signature`
    /// updates the tracked device fingerprint when the caller already
    /// computed it (`None` recomputes it — the dead-capture path, where no
    /// device change was detected).
    fn rebuild_engine(&mut self, reason: String, new_signature: Option<String>) {
        let Some(factory) = &self.engine_factory else {
            return;
        };
        match factory() {
            Ok(new_engine) => {
                self.engine = new_engine;
                self.device_signature = Some(new_signature.unwrap_or_else(|| {
                    self.signature_mask
                        .apply(&buzztalk_audio::default_devices_signature())
                }));
                self.rebuild_failures = 0;
                self.aec = if self.no_aec {
                    Box::new(NullCanceller)
                } else {
                    new_best_available()
                };
                self.resampler_16k.reset();
                self.endpoint.reset();
                self.bargein.reset();
                if let Some(stt) = &self.stt {
                    stt.reset();
                }
                self.stt_active_turn = None;
                self.pending_ref.clear();
                self.pending_cap.clear();
                self.playback_pushed_samples = 0;
                self.playback_push_started_at = None;
                self.mic_silence_elapsed = Duration::ZERO;
                self.mic_stall_reported = false;
                self.consecutive_empty_finals = 0;
                let _ = self
                    .events_tx
                    .send(PipelineEvent::AudioDeviceRebuilt { reason });
                // End-then-start so a turn never straddles two engines.
                self.dispatch(Input::SessionEnd);
                self.dispatch(Input::SessionStart);
            }
            Err(err) => {
                self.rebuild_failures += 1;
                // First failure and every ~10s thereafter, not every tick.
                if self.rebuild_failures == 1 || self.rebuild_failures.is_multiple_of(10) {
                    let _ = self.events_tx.send(PipelineEvent::CapabilityLost {
                        what: "audio-device",
                        reason: format!("engine rebuild failed ({err}); will keep retrying"),
                    });
                }
            }
        }
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
            if let Some(tts) = &self.tts {
                tts.cancel(turn);
            }
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
                // If STT is unavailable, nothing will ever call
                // `dispatch(Input::FinalTranscript { .. })` for this turn --
                // there's simply no source for one. That's fine, not a
                // hang: `buzztalk_session`'s own `FINALIZE_TIMEOUT` already
                // bounds how long `Finalizing` waits before giving up and
                // returning to `Listening` on its own, driven by the
                // `Input::Tick`s this loop keeps sending regardless.
                if let (Some(stt), Some(turn)) = (&self.stt, self.session.current_turn()) {
                    stt.finish_utterance(turn);
                }
            }
            SessionState::Interrupting => {
                self.bargein.notify_playback_stopped();
            }
            SessionState::Idle => {
                self.endpoint.reset();
                self.bargein.reset();
                if let Some(stt) = &self.stt {
                    stt.reset();
                }
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
            if let Some(stt) = &self.stt {
                stt.push_preroll_audio(turn, samples_16k);
            }
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
                // Dead-capture watchdog. A final with no alphanumeric
                // content that nonetheless closed a real speech turn means
                // the endpointer heard energy but the recognizer got no
                // words -- the signature of a Bluetooth capture stream that
                // has silently gone bad (rate renegotiated under us, or the
                // headset's mic link dropped) while still delivering frames,
                // so neither the stream-error nor device-signature nor
                // stall triggers fire. A run of these means rebuild the
                // engine; a single one is just a cough or a false VAD
                // trigger and is ignored. A word-bearing final clears it.
                if text.chars().any(|c| c.is_alphanumeric()) {
                    self.consecutive_empty_finals = 0;
                } else {
                    self.consecutive_empty_finals += 1;
                    if self.consecutive_empty_finals >= DEAD_CAPTURE_EMPTY_FINALS
                        && self.simulate_frames.is_none()
                        && self.engine_factory.is_some()
                    {
                        self.rebuild_engine(
                            format!(
                                "capture producing no words for {} turns (stream gone bad)",
                                self.consecutive_empty_finals
                            ),
                            None,
                        );
                    }
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
                    // Unconditional even with no TTS -- see
                    // `nothing_left_to_play`, which reads this same state to
                    // decide the turn has nothing left to wait for.
                    self.playback.start(turn);
                    self.bargein.notify_playback_started();
                    self.bargein.reset();
                }
                let _ = self.events_tx.send(PipelineEvent::AgentText(text.clone()));
                // Route through `dispatch`, not a bare `session.handle`
                // discarding the result: on the first chunk this is the
                // AwaitingAgent -> AgentSpeaking transition, and
                // `SessionMachine::handle` auto-appends `Output::EmitState`
                // for every state change, which `act_all` turns into
                // `PipelineEvent::StateChanged(AgentSpeaking)` -- dropping
                // it here (as this used to) silently withheld exactly the
                // state transition this crate's degraded-capability report
                // and tests most need visibility into.
                self.dispatch(Input::AgentTextArrived {
                    turn,
                    text: text.clone(),
                });

                // No TTS: there is nothing to synthesize with, so the reply
                // stays text-only (`PipelineEvent::AgentText` above already
                // covers it) -- see `nothing_left_to_play`, called from
                // `AgentTurnComplete` below, for how the turn still
                // terminates instead of AgentSpeaking waiting forever on
                // audio that will never come.
                if let Some(tts) = &self.tts {
                    for phrase in buzztalk_tts::segment_into_phrases(&text) {
                        tts.synthesize(turn, phrase);
                        *self.tts_pending.entry(turn).or_insert(0) += 1;
                    }
                }
            }
            AgentEvent::TurnComplete { turn } => {
                self.dispatch(Input::AgentTurnComplete { turn });
                // Usually a no-op (see `on_playback_drained`'s own
                // `agent_turn_complete` gate): if real audio is still
                // playing or still being synthesized, nothing happens here
                // and `feed_playback`'s own drain-edge finishes the turn
                // once it truly has nothing left. But when there was never
                // going to be any audio at all for this turn (no TTS, or
                // every synthesis call for it failed), nothing will ever
                // trip that edge, so the turn is completed right here
                // instead of leaving `AgentSpeaking` waiting forever on a
                // `PlaybackDrained` that can never arrive.
                if self.nothing_left_to_play(turn) {
                    self.signal_drained(turn);
                }
            }
        }
    }

    /// Whether `turn` has no audio currently queued, in flight to the
    /// engine, or still being synthesized -- i.e. whether it's safe to tell
    /// the session machine this turn's playback is drained.
    fn nothing_left_to_play(&self, turn: TurnId) -> bool {
        if !self.playback.recognises(turn) {
            // Never started (no TTS ever ran for it) or already abandoned
            // (a barge-in cancelled it) -- either way, nothing to wait for.
            return true;
        }
        let pending = self.tts_pending.get(&turn).copied().unwrap_or(0);
        pending == 0
            && self.playback.is_live(turn)
            && self.playback.live_len() == 0
            && self.resident_playback_samples() == 0
    }

    /// Estimate of how many playback samples are still resident in the
    /// engine's ring right now (already pushed, not yet played) -- the same
    /// "how much is probably still queued" math [`Self::feed_playback`]
    /// uses to decide how much more to top up.
    fn resident_playback_samples(&self) -> u64 {
        let Some(anchor) = self.playback_push_started_at else {
            return 0;
        };
        let elapsed_samples =
            (anchor.elapsed().as_secs_f64() * buzztalk_core::SAMPLE_RATE_HZ as f64) as u64;
        self.playback_pushed_samples.saturating_sub(elapsed_samples)
    }

    /// Tell the session machine `turn`'s playback has drained -- the only
    /// thing that can move `AgentSpeaking` forward once its text is
    /// complete (see `buzztalk_session::SessionMachine::on_playback_drained`).
    /// Safe to call speculatively: a stale or not-yet-complete turn is a
    /// no-op there.
    fn signal_drained(&mut self, turn: TurnId) {
        self.tts_pending.remove(&turn);
        self.dispatch(Input::PlaybackDrained { turn });
    }

    // ── Audio pump ────────────────────────────────────────────────────────

    fn pump_audio(&mut self) {
        self.saw_capture_frame_this_tick = false;

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
                self.saw_capture_frame_this_tick = true;
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

        if let (Some(turn), Some(stt)) = (self.stt_active_turn, &self.stt) {
            if matches!(
                self.session.state(),
                SessionState::UserSpeaking | SessionState::Interrupting
            ) {
                let samples_16k = self.resampler_16k.process(near);
                if !samples_16k.is_empty() && !stt.push_audio(turn, samples_16k) {
                    let _ = self.events_tx.send(PipelineEvent::Dropped {
                        what: "STT audio (recognizer fell behind)",
                        total: stt.dropped_count(),
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
            // The ring has genuinely gone dry. If there's still a
            // synthesis request in flight for the live turn, this is
            // exactly the "buffer ran dry ahead of synthesis" underrun
            // `SessionMachine::on_playback_drained`'s docs call out as
            // harmless -- signalling it anyway is safe (it only ends the
            // turn if `AgentTurnComplete` already arrived), but skip it
            // when there's known-more-coming so a same-turn race with
            // `nothing_left_to_play` can't end the turn one phrase early.
            if let Some(turn) = self.playback.live_turn() {
                let pending = self.tts_pending.get(&turn).copied().unwrap_or(0);
                if pending == 0 {
                    self.signal_drained(turn);
                }
            }
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

    use super::*;

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

    // ── Failure-path tests: capabilities, degraded operation, no hangs ──
    //
    // Everything below builds an `Orchestrator` directly (via
    // `build_orchestrator`) instead of going through
    // `ConversationPipeline::start`, which opens a real `cpal` device --
    // not available in CI, and not needed to exercise any of this crate's
    // own degradation logic. In its place: `FakeAudioEngine` (a scripted
    // `AudioEngine`, see `audio_engine::tests`), an `SttWorker`/`TtsWorker`
    // spawned around a tiny in-process fake where a real worker thread is
    // wanted at all, and a hand-driven `ScriptedAgent` instead of
    // `EchoAgent`. No model files, no audio device, anywhere in this
    // section.

    use crate::audio_engine::tests::{FakeAudioEngine, FakeAudioEngineHandle};
    use buzztalk_core::Result as CoreResult;
    use buzztalk_stt::{SpeechRecognizer, Transcript};
    use buzztalk_tts::{AudioChunk, Result as TtsResultT, SpeechSynthesizer};
    use std::sync::{Arc, Mutex};

    /// A recognizer that hands back a fixed final transcript on
    /// `finish_utterance`, ignoring whatever audio was actually pushed --
    /// stands in for a working STT engine in tests that care about the
    /// pipeline's wiring, not real speech recognition.
    struct EchoRecognizer {
        text: &'static str,
    }
    impl SpeechRecognizer for EchoRecognizer {
        fn push_audio(&mut self, _samples: &[f32]) -> CoreResult<Option<Transcript>> {
            Ok(None)
        }
        fn finish_utterance(&mut self) -> CoreResult<Option<Transcript>> {
            Ok(Some(Transcript::Final {
                text: self.text.to_string(),
            }))
        }
        fn reset(&mut self) {}
    }

    /// A recognizer that panics on `finish_utterance` -- standing in for an
    /// engine thread dying mid-session. See
    /// `stt_worker::tests::a_panicking_worker_thread_is_observable_as_disconnected_not_a_hang`
    /// for the same idea at the worker level; this is the pipeline-level
    /// version, proving the orchestrator itself survives it.
    struct PanicOnFinishRecognizer;
    impl SpeechRecognizer for PanicOnFinishRecognizer {
        fn push_audio(&mut self, _samples: &[f32]) -> CoreResult<Option<Transcript>> {
            Ok(None)
        }
        fn finish_utterance(&mut self) -> CoreResult<Option<Transcript>> {
            panic!("stt engine fell over");
        }
        fn reset(&mut self) {}
    }

    /// A synthesizer that turns every phrase into a fixed, tiny burst of
    /// audio -- enough to prove synthesized audio actually reaches the
    /// (fake) engine, without the real Pocket TTS bundle.
    struct FakeSynthesizer;
    impl SpeechSynthesizer for FakeSynthesizer {
        fn synthesize(&mut self, _text: &str) -> TtsResultT<AudioChunk> {
            Ok(AudioChunk {
                samples: vec![0.1; 64],
                sample_rate: 24_000,
                chunk_index: 1,
            })
        }
        fn warmup(&mut self) -> TtsResultT<()> {
            Ok(())
        }
    }

    /// An [`AgentBackend`] the test drives by hand: `submit` just records
    /// what it was given, and replies are whatever the test pushes through
    /// [`ScriptedAgentHandle`] -- no real thread, no timing dependency,
    /// fully deterministic.
    struct ScriptedAgent {
        submits: Arc<Mutex<Vec<(TurnId, String)>>>,
        events_rx: Receiver<AgentEvent>,
    }

    #[derive(Clone)]
    struct ScriptedAgentHandle {
        submits: Arc<Mutex<Vec<(TurnId, String)>>>,
        events_tx: Sender<AgentEvent>,
    }

    impl ScriptedAgent {
        fn new() -> (Self, ScriptedAgentHandle) {
            let submits = Arc::new(Mutex::new(Vec::new()));
            let (events_tx, events_rx) = mpsc::channel();
            (
                Self {
                    submits: Arc::clone(&submits),
                    events_rx,
                },
                ScriptedAgentHandle { submits, events_tx },
            )
        }
    }

    impl AgentBackend for ScriptedAgent {
        fn submit(&mut self, turn: TurnId, text: &str) {
            self.submits.lock().unwrap().push((turn, text.to_string()));
        }

        fn poll(&mut self) -> Option<AgentEvent> {
            self.events_rx.try_recv().ok()
        }
    }

    impl ScriptedAgentHandle {
        fn submits(&self) -> Vec<(TurnId, String)> {
            self.submits.lock().unwrap().clone()
        }

        fn say(&self, turn: TurnId, text: &str) {
            let _ = self.events_tx.send(AgentEvent::TextChunk {
                turn,
                text: text.to_string(),
            });
        }

        fn complete(&self, turn: TurnId) {
            let _ = self.events_tx.send(AgentEvent::TurnComplete { turn });
        }
    }

    /// Build an `Orchestrator` directly around fakes -- the test
    /// construction path this crate needed to make its failure paths
    /// testable at all. Mirrors `ConversationPipeline::start`'s wiring but
    /// takes the engine/STT/TTS/agent as parameters instead of constructing
    /// the real ones, and forces the output route so no test depends on
    /// real output-route detection.
    fn build_orchestrator(
        engine: Box<dyn AudioEngine>,
        stt: Option<SttWorker>,
        tts: Option<TtsWorker>,
        agent: Box<dyn AgentBackend>,
    ) -> (Orchestrator, Receiver<PipelineEvent>) {
        let capabilities = Capabilities {
            stt: stt.is_some(),
            tts: tts.is_some(),
            stt_unavailable_reason: stt.is_none().then(|| "test: stt not spawned".to_string()),
            tts_unavailable_reason: tts.is_none().then(|| "test: tts not spawned".to_string()),
        };
        let (_control_tx, control_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let orchestrator = Orchestrator {
            engine,
            engine_factory: None,
            device_signature: None,
            signature_mask: SignatureMask {
                input: true,
                output: true,
            },
            device_check_elapsed: Duration::ZERO,
            rebuild_failures: 0,
            stall_rebuild_attempted: false,
            consecutive_empty_finals: 0,
            no_aec: true,
            aec: Box::new(NullCanceller),
            endpoint: EndpointDetector::default(),
            bargein: BargeInDetector::new(Default::default()),
            session: SessionMachine::new(),
            resampler_16k: Resampler48to16::new(),
            stt,
            tts,
            agent,
            playback: PlaybackState::new(),
            capabilities,
            forced_output_route: Some(OutputRoute::Headphones),
            simulate_frames: None,
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
            mic_silence_elapsed: Duration::ZERO,
            mic_stall_reported: false,
            saw_capture_frame_this_tick: false,
            tts_pending: HashMap::new(),
            events_tx,
            control_rx,
            running: true,
        };
        (orchestrator, events_rx)
    }

    /// Offer one healthy frame pair and tick `n` times, a small fixed `dt`
    /// per tick -- the "normal operation" pacing most tests just need to
    /// let async worker results and control-flow settle.
    fn drive_ticks(orch: &mut Orchestrator, handle: &FakeAudioEngineHandle, n: usize) {
        for _ in 0..n {
            handle.offer_frame_pair();
            orch.tick(Duration::from_millis(5));
        }
    }

    /// Tick up to `max_ticks` times (offering a healthy frame pair each
    /// time, with a short real sleep so a real worker thread gets a chance
    /// to answer), stopping as soon as `done` is satisfied. Returns whether
    /// it ever was -- this is the "bounded number of ticks" proof itself:
    /// a hang shows up here as `false`, not as the test never returning.
    fn drive_until(
        orch: &mut Orchestrator,
        handle: &FakeAudioEngineHandle,
        max_ticks: usize,
        mut done: impl FnMut(&Orchestrator) -> bool,
    ) -> bool {
        for _ in 0..max_ticks {
            if done(orch) {
                return true;
            }
            handle.offer_frame_pair();
            orch.tick(Duration::from_millis(5));
            thread::sleep(Duration::from_millis(2));
        }
        done(orch)
    }

    /// Drain every event currently queued, returning them in order --
    /// `try_recv` never blocks, so this is safe to call any time.
    fn drain_events(events_rx: &Receiver<PipelineEvent>) -> Vec<PipelineEvent> {
        let mut out = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            out.push(event);
        }
        out
    }

    #[test]
    fn stt_missing_at_startup_reports_it_and_agent_replies_still_produce_speech_events() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let tts = TtsWorker::spawn_with(FakeSynthesizer);
        let (agent, agent_handle) = ScriptedAgent::new();
        let (mut orch, events_rx) =
            build_orchestrator(Box::new(engine), None, Some(tts), Box::new(agent));

        orch.emit_capabilities();
        match events_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(PipelineEvent::Capabilities(caps)) => {
                assert!(!caps.stt, "stt must be reported unavailable");
                assert!(caps.tts, "tts must be reported available");
                let report = caps.report().to_lowercase();
                assert!(
                    report.contains("speech-to-text") || report.contains("stt"),
                    "capability report must say what's missing: {report:?}"
                );
            }
            other => panic!("expected a Capabilities event first, got {other:?}"),
        }

        // No STT worker will ever call `Input::FinalTranscript` on its own;
        // stand in for whatever else could still submit an utterance (a
        // future text/telephony transport, or here, PTT + a hand-fed
        // transcript) to prove the *rest* of the pipeline -- submit ->
        // agent -> TTS -> playback -- doesn't care that STT is the piece
        // that's missing.
        orch.dispatch(Input::SessionStart);
        orch.dispatch(Input::PushToTalkPressed);
        let turn = orch
            .session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased);
        orch.dispatch(Input::FinalTranscript {
            turn,
            text: "hello".into(),
        });
        assert_eq!(orch.session.state(), SessionState::AwaitingAgent);

        agent_handle.say(turn, "hi there");
        drive_ticks(&mut orch, &engine_handle, 5);
        assert_eq!(orch.session.state(), SessionState::AgentSpeaking);

        agent_handle.complete(turn);
        // Drive on the turn's actual completion, not on audio having
        // reached the engine -- that can (and, with a synthesizer this
        // fast, does) already be true from before `complete` was even
        // called, which would make a `push_playback_calls() > 0` predicate
        // satisfied immediately without ever giving `TurnComplete` a chance
        // to be polled.
        let done = drive_until(&mut orch, &engine_handle, 100, |o| {
            o.session.state() == SessionState::Listening
        });
        assert!(done, "the turn must still complete with STT unavailable");
        assert!(
            engine_handle.push_playback_calls() > 0,
            "synthesized speech must have reached the engine even with STT unavailable"
        );
    }

    #[test]
    fn tts_missing_at_startup_reports_it_and_transcription_still_submits_text_only_replies() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let stt = SttWorker::spawn_with(EchoRecognizer {
            text: "hello agent",
        });
        let (agent, agent_handle) = ScriptedAgent::new();
        let (mut orch, events_rx) =
            build_orchestrator(Box::new(engine), Some(stt), None, Box::new(agent));

        orch.emit_capabilities();
        match events_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(PipelineEvent::Capabilities(caps)) => {
                assert!(caps.stt, "stt must be reported available");
                assert!(!caps.tts, "tts must be reported unavailable");
            }
            other => panic!("expected a Capabilities event first, got {other:?}"),
        }

        orch.dispatch(Input::SessionStart);
        orch.dispatch(Input::PushToTalkPressed);
        let turn = orch
            .session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased);
        assert_eq!(orch.session.state(), SessionState::Finalizing);

        // The real (fake-backed) STT worker thread needs a moment to answer.
        let submitted = drive_until(&mut orch, &engine_handle, 200, |o| {
            o.session.state() == SessionState::AwaitingAgent
        });
        assert!(submitted, "the utterance must still be submitted");
        assert_eq!(
            agent_handle.submits(),
            vec![(turn, "hello agent".to_string())],
            "transcription must reach the agent even with TTS unavailable"
        );

        agent_handle.say(turn, "reply text, no voice");
        agent_handle.complete(turn);
        let done = drive_until(&mut orch, &engine_handle, 100, |o| {
            o.session.state() == SessionState::Listening
        });
        assert!(done, "the turn must complete even with nothing to play");
        assert_eq!(
            engine_handle.push_playback_calls(),
            0,
            "no audio should ever be pushed with TTS unavailable"
        );

        let events = drain_events(&events_rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PipelineEvent::AgentText(t) if t == "reply text, no voice")),
            "the agent's reply must still arrive as a text event: {events:?}"
        );
    }

    #[test]
    fn neither_engine_available_still_starts_reports_both_missing_and_does_not_hang() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let (agent, agent_handle) = ScriptedAgent::new();
        let (mut orch, events_rx) =
            build_orchestrator(Box::new(engine), None, None, Box::new(agent));

        orch.emit_capabilities();
        match events_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(PipelineEvent::Capabilities(caps)) => {
                assert!(!caps.stt);
                assert!(!caps.tts);
                let report = caps.report();
                assert!(
                    !report.is_empty(),
                    "the report must say something plain when both are missing"
                );
            }
            other => panic!("expected a Capabilities event first, got {other:?}"),
        }

        orch.dispatch(Input::SessionStart);
        orch.dispatch(Input::PushToTalkPressed);
        let turn = orch
            .session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased);
        orch.dispatch(Input::FinalTranscript {
            turn,
            text: "hello".into(),
        });
        assert_eq!(orch.session.state(), SessionState::AwaitingAgent);

        agent_handle.say(turn, "hi");
        agent_handle.complete(turn);
        let done = drive_until(&mut orch, &engine_handle, 20, |o| {
            o.session.state() == SessionState::Listening
        });
        assert!(
            done,
            "the pipeline must not hang even with neither engine available"
        );
    }

    #[test]
    fn agent_speaking_with_tts_unavailable_reaches_a_terminal_state_within_bounded_ticks() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let (agent, agent_handle) = ScriptedAgent::new();
        let (mut orch, _events_rx) =
            build_orchestrator(Box::new(engine), None, None, Box::new(agent));

        orch.dispatch(Input::SessionStart);
        orch.dispatch(Input::PushToTalkPressed);
        let turn = orch
            .session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased);
        orch.dispatch(Input::FinalTranscript {
            turn,
            text: "hello".into(),
        });

        agent_handle.say(turn, "a reply with no audio behind it");
        drive_ticks(&mut orch, &engine_handle, 3);
        assert_eq!(orch.session.state(), SessionState::AgentSpeaking);

        agent_handle.complete(turn);

        const BOUND: usize = 10;
        let mut terminal_within_bound = false;
        for _ in 0..BOUND {
            engine_handle.offer_frame_pair();
            orch.tick(Duration::from_millis(5));
            if orch.session.state() == SessionState::Listening {
                terminal_within_bound = true;
                break;
            }
        }
        assert!(
            terminal_within_bound,
            "AgentSpeaking must not wait forever for a PlaybackDrained that will never come; \
             state after {BOUND} ticks was {:?}",
            orch.session.state()
        );
    }

    #[test]
    fn stt_worker_thread_dying_mid_session_is_detected_reported_and_the_session_survives() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let stt = SttWorker::spawn_with(PanicOnFinishRecognizer);
        let tts = TtsWorker::spawn_with(FakeSynthesizer);
        let (agent, agent_handle) = ScriptedAgent::new();
        let (mut orch, events_rx) =
            build_orchestrator(Box::new(engine), Some(stt), Some(tts), Box::new(agent));

        orch.dispatch(Input::SessionStart);
        orch.dispatch(Input::PushToTalkPressed);
        orch.session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased); // triggers finish_utterance -> worker panics
        assert_eq!(orch.session.state(), SessionState::Finalizing);

        let lost = drive_until(&mut orch, &engine_handle, 200, |o| o.stt.is_none());
        assert!(
            lost,
            "a panicking STT worker thread must be detected, not hang forever"
        );

        let saw_capability_lost = drain_events(&events_rx).into_iter().any(|e| {
            matches!(
                e,
                PipelineEvent::CapabilityLost {
                    what: "speech-to-text",
                    ..
                }
            )
        });
        assert!(saw_capability_lost, "the host must be told STT was lost");

        // The session survives: `Finalizing` has nothing left to wait for
        // (no STT, ever again) and returns to `Listening` on its own
        // timeout, driven by the same `Input::Tick`s this loop keeps
        // sending regardless of what died.
        let mut recovered = false;
        for _ in 0..30 {
            engine_handle.offer_frame_pair();
            orch.tick(Duration::from_millis(200)); // 30 * 200ms = 6s > FINALIZE_TIMEOUT (5s)
            if orch.session.state() == SessionState::Listening {
                recovered = true;
                break;
            }
        }
        assert!(
            recovered,
            "the pipeline must recover to Listening, not get stuck"
        );

        // And the rest of the pipeline keeps working: a new turn, submitted
        // the same stand-in way as the "STT missing from the start" tests,
        // still makes it all the way to spoken output on the TTS that's
        // still alive.
        orch.dispatch(Input::PushToTalkPressed);
        let turn2 = orch
            .session
            .current_turn()
            .expect("ptt press starts a turn");
        orch.dispatch(Input::PushToTalkReleased);
        orch.dispatch(Input::FinalTranscript {
            turn: turn2,
            text: "still alive".into(),
        });
        assert_eq!(orch.session.state(), SessionState::AwaitingAgent);

        agent_handle.say(turn2, "yep");
        agent_handle.complete(turn2);
        let handle_for_predicate = engine_handle.clone();
        let spoke = drive_until(&mut orch, &engine_handle, 100, move |_| {
            handle_for_predicate.push_playback_calls() > 0
        });
        assert!(spoke, "TTS must still work after STT died mid-session");
    }

    #[test]
    fn microphone_going_silent_is_detected_reported_once_and_does_not_hang_or_spin() {
        let (engine, engine_handle) = FakeAudioEngine::new();
        let (agent, _agent_handle) = ScriptedAgent::new();
        let (mut orch, events_rx) =
            build_orchestrator(Box::new(engine), None, None, Box::new(agent));

        // Healthy device: frames keep arriving, no stall reported.
        for _ in 0..20 {
            engine_handle.offer_frame_pair();
            orch.tick(Duration::from_millis(10));
        }
        assert!(
            drain_events(&events_rx).iter().all(|e| !matches!(
                e,
                PipelineEvent::CapabilityLost {
                    what: "microphone",
                    ..
                }
            )),
            "a healthy mic must not be reported lost"
        );

        // Mic "unplugged": capture stops, output keeps running -- past
        // MIC_STALL_TIMEOUT (1.5s), spread over several ticks so a hot-loop
        // regression would show up as many events, not zero or one by luck.
        let mut lost_events = 0;
        for _ in 0..10 {
            engine_handle.offer_render_only();
            orch.tick(Duration::from_millis(300)); // 10 * 300ms = 3s total
            lost_events += drain_events(&events_rx)
                .into_iter()
                .filter(|e| {
                    matches!(
                        e,
                        PipelineEvent::CapabilityLost {
                            what: "microphone",
                            ..
                        }
                    )
                })
                .count();
        }
        assert_eq!(
            lost_events, 1,
            "the microphone must be reported lost exactly once, not spammed"
        );
    }
}
