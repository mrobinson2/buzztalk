//! The STT worker thread: owns a [`ParakeetRecognizer`] so re-decode
//! (CPU-bound, measured up to ~700 ms worst case) never runs on the
//! audio-frame loop.
//!
//! The steady stream of per-frame audio from the orchestration loop uses a
//! bounded channel and [`SttWorker::push_audio`], which never blocks: if
//! the recognizer has fallen behind, the push is dropped and counted
//! instead. `finish_utterance`/`reset` are rare, turn-boundary events, not
//! part of the per-frame budget, so they use a blocking send -- losing one
//! of those would silently strand a turn.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use buzztalk_session::TurnId;
use buzztalk_stt::{ModelError, ParakeetRecognizer, SpeechRecognizer, Transcript};

/// Bound on the audio-push channel from the orchestration loop to this
/// worker. Small and finite on purpose: if the recognizer falls behind, the
/// orchestration loop must drop and count rather than block (it's on the
/// real-time audio path), and a deep queue would just turn a stall into
/// unbounded latency instead of a visible, counted drop.
const AUDIO_QUEUE_CAPACITY: usize = 64;

/// A request for the STT worker.
enum SttRequest {
    /// More 16 kHz mono audio for `turn`'s in-progress utterance.
    PushAudio { turn: TurnId, samples: Vec<f32> },
    /// End `turn`'s utterance and emit a final transcript.
    FinishUtterance { turn: TurnId },
    /// Discard all buffered audio without emitting anything (session end).
    Reset,
}

/// A result from the STT worker, always tagged with the turn it belongs to
/// so the orchestration loop can hand it straight to
/// `SessionMachine::handle` as an `Input`.
pub(crate) enum SttResult {
    /// An interim hypothesis for the in-progress utterance.
    Partial { turn: TurnId, text: String },
    /// The finalized transcript for a closed utterance.
    Final { turn: TurnId, text: String },
}

/// A running STT worker: the recognizer lives entirely on its own thread.
pub(crate) struct SttWorker {
    // `Option` so `Drop` can explicitly close the channel (drop the sender)
    // *before* joining the worker thread. A struct's own `Drop::drop` runs
    // before its fields' automatic drops, so without this, `handle.join()`
    // would wait on a `request_rx` that's still open from the worker
    // thread's point of view -- the worker's `for request in request_rx`
    // loop would then block forever waiting for a message that will never
    // arrive, and the join (and the whole shutdown) would hang.
    request_tx: Option<SyncSender<SttRequest>>,
    result_rx: Receiver<SttResult>,
    handle: Option<JoinHandle<()>>,
    audio_dropped: Arc<AtomicU64>,
}

impl SttWorker {
    /// Load the default Parakeet model and spawn the worker thread.
    pub(crate) fn spawn() -> Result<Self, ModelError> {
        let recognizer = ParakeetRecognizer::with_default_model()?;
        Ok(Self::spawn_with(recognizer))
    }

    /// Spawn the worker around an already-constructed recognizer. Exists
    /// mainly so tests can drive the worker's request/result plumbing
    /// against a lightweight fake, without needing the real model on disk.
    pub(crate) fn spawn_with(mut recognizer: impl SpeechRecognizer + 'static) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel::<SttRequest>(AUDIO_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::channel::<SttResult>();
        let audio_dropped = Arc::new(AtomicU64::new(0));

        let handle = thread::Builder::new()
            .name("buzztalk-stt".into())
            .spawn(move || {
                for request in request_rx {
                    match request {
                        SttRequest::PushAudio { turn, samples } => {
                            if let Ok(Some(Transcript::Partial { text, .. })) =
                                recognizer.push_audio(&samples)
                            {
                                if result_tx.send(SttResult::Partial { turn, text }).is_err() {
                                    return;
                                }
                            }
                        }
                        SttRequest::FinishUtterance { turn } => {
                            if let Ok(Some(Transcript::Final { text })) =
                                recognizer.finish_utterance()
                            {
                                if result_tx.send(SttResult::Final { turn, text }).is_err() {
                                    return;
                                }
                            }
                        }
                        SttRequest::Reset => recognizer.reset(),
                    }
                }
            })
            .expect("failed to spawn buzztalk-stt worker thread");

        Self {
            request_tx: Some(request_tx),
            result_rx,
            handle: Some(handle),
            audio_dropped,
        }
    }

    /// Non-blocking push of one chunk of 16 kHz audio for `turn`. Returns
    /// `false` (and counts the drop) if the worker's queue is full or the
    /// worker has exited -- the audio-frame loop must never block on this.
    pub(crate) fn push_audio(&self, turn: TurnId, samples: Vec<f32>) -> bool {
        let Some(tx) = &self.request_tx else {
            return false;
        };
        match tx.try_send(SttRequest::PushAudio { turn, samples }) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.audio_dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Blocking send of a one-shot burst of pre-roll audio for `turn` --
    /// see `Output::ReplayPreRoll`. Unlike the steady per-frame stream,
    /// dropping this would silently lose the word pre-roll exists to save,
    /// so this is allowed to briefly wait rather than drop.
    pub(crate) fn push_preroll_audio(&self, turn: TurnId, samples: Vec<f32>) {
        if let Some(tx) = &self.request_tx {
            let _ = tx.send(SttRequest::PushAudio { turn, samples });
        }
    }

    /// End `turn`'s utterance and request a final transcript. Blocking:
    /// this is a rare, turn-boundary event, not part of the per-frame
    /// audio budget.
    pub(crate) fn finish_utterance(&self, turn: TurnId) {
        if let Some(tx) = &self.request_tx {
            let _ = tx.send(SttRequest::FinishUtterance { turn });
        }
    }

    /// Discard all buffered audio (session end). Blocking, same reasoning
    /// as [`Self::finish_utterance`].
    pub(crate) fn reset(&self) {
        if let Some(tx) = &self.request_tx {
            let _ = tx.send(SttRequest::Reset);
        }
    }

    /// Non-blocking pop of the next available result.
    pub(crate) fn try_recv_result(&self) -> Option<SttResult> {
        self.result_rx.try_recv().ok()
    }

    /// Total audio pushes dropped so far because the worker's queue was
    /// full (the recognizer fell behind).
    pub(crate) fn dropped_count(&self) -> u64 {
        self.audio_dropped.load(Ordering::Relaxed)
    }
}

impl Drop for SttWorker {
    fn drop(&mut self) {
        // Explicitly drop the sender first: this closes the channel, which
        // is what ends the worker's `for request in request_rx` loop. Only
        // then is it safe to join without risking a hang (see the field
        // doc on `request_tx`).
        drop(self.request_tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_core::Result as CoreResult;
    use buzztalk_session::{Input, SessionMachine};
    use std::time::{Duration, Instant};

    fn a_turn() -> TurnId {
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        machine.handle(Input::PushToTalkPressed);
        machine.current_turn().unwrap()
    }

    /// A trivial fake recognizer so worker-plumbing tests don't need the
    /// real (125 MB) Parakeet model on disk. Echoes back whatever text was
    /// pushed as a "transcript", once per boundary, so we can assert on
    /// message flow without touching sherpa-onnx at all.
    struct FakeRecognizer {
        pushed_word_count: usize,
    }

    impl SpeechRecognizer for FakeRecognizer {
        fn push_audio(&mut self, samples_16k: &[f32]) -> CoreResult<Option<Transcript>> {
            if samples_16k.is_empty() {
                return Ok(None);
            }
            self.pushed_word_count += 1;
            Ok(Some(Transcript::Partial {
                text: format!("partial-{}", self.pushed_word_count),
                stable_prefix_len: 0,
            }))
        }

        fn finish_utterance(&mut self) -> CoreResult<Option<Transcript>> {
            if self.pushed_word_count == 0 {
                return Ok(None);
            }
            let text = format!("final-{}", self.pushed_word_count);
            self.pushed_word_count = 0;
            Ok(Some(Transcript::Final { text }))
        }

        fn reset(&mut self) {
            self.pushed_word_count = 0;
        }
    }

    fn recv_within(worker: &SttWorker, timeout: Duration) -> Option<SttResult> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(r) = worker.try_recv_result() {
                return Some(r);
            }
            thread::sleep(Duration::from_millis(2));
        }
        None
    }

    #[test]
    fn push_audio_then_finish_produces_a_tagged_final_transcript() {
        let worker = SttWorker::spawn_with(FakeRecognizer {
            pushed_word_count: 0,
        });
        let turn = a_turn();

        assert!(worker.push_audio(turn, vec![0.1; 100]));
        worker.finish_utterance(turn);

        let mut saw_final = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match recv_within(&worker, Duration::from_millis(50)) {
                Some(SttResult::Final { turn: t, text }) => {
                    assert_eq!(t, turn);
                    assert_eq!(text, "final-1");
                    saw_final = true;
                    break;
                }
                Some(SttResult::Partial { turn: t, .. }) => assert_eq!(t, turn),
                None => {}
            }
        }
        assert!(
            saw_final,
            "expected a Final transcript tagged with the pushed turn"
        );
    }

    #[test]
    fn finish_with_no_audio_pushed_yields_nothing() {
        let worker = SttWorker::spawn_with(FakeRecognizer {
            pushed_word_count: 0,
        });
        let turn = a_turn();
        worker.finish_utterance(turn);
        assert!(recv_within(&worker, Duration::from_millis(200)).is_none());
    }

    #[test]
    fn queue_overrun_drops_and_counts_instead_of_blocking() {
        // A recognizer that blocks on a shared gate until the test releases
        // it, so the queue backs up and the drop path is actually
        // exercised -- without needing a real fixed-duration sleep that
        // `SttWorker::drop`'s join would then have to sit through once per
        // buffered request (a naive `thread::sleep` here made this test's
        // teardown take minutes: the worker had to drain ~65 queued
        // requests sequentially before the channel closed).
        struct GatedRecognizer {
            released: Arc<std::sync::atomic::AtomicBool>,
        }
        impl SpeechRecognizer for GatedRecognizer {
            fn push_audio(&mut self, _samples: &[f32]) -> CoreResult<Option<Transcript>> {
                while !self.released.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(None)
            }
            fn finish_utterance(&mut self) -> CoreResult<Option<Transcript>> {
                Ok(None)
            }
            fn reset(&mut self) {}
        }

        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = SttWorker::spawn_with(GatedRecognizer {
            released: Arc::clone(&released),
        });
        let turn = a_turn();
        // First push starts the gated block on the worker thread; every
        // push after that queues up until AUDIO_QUEUE_CAPACITY is exhausted,
        // then must start reporting drops rather than blocking this test
        // thread.
        let mut drops = 0;
        for _ in 0..(AUDIO_QUEUE_CAPACITY + 16) {
            if !worker.push_audio(turn, vec![0.1; 10]) {
                drops += 1;
            }
        }
        assert!(
            drops > 0,
            "expected at least one drop once the queue filled"
        );
        assert_eq!(worker.dropped_count(), drops);

        // Release the gate before `worker` drops: `SttWorker::drop` joins
        // the worker thread, which won't happen until every buffered
        // request has been drained.
        released.store(true, Ordering::Relaxed);
    }
}
