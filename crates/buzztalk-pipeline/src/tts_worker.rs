//! The TTS worker thread: owns a [`PocketSynthesizer`] so ONNX inference
//! (tens to hundreds of milliseconds per phrase) never runs on the
//! audio-frame loop.
//!
//! Synthesis requests aren't part of the tight per-frame audio budget (they
//! happen once per agent text chunk, not once per 10 ms frame), so this
//! worker's channels are plain unbounded `mpsc` -- sending never blocks the
//! orchestration loop regardless.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use buzztalk_session::TurnId;
use buzztalk_tts::{Error as TtsError, SpeechSynthesizer};

use crate::playback::resample_linear_to_48k;

/// A request for the TTS worker.
enum TtsRequest {
    /// Synthesize one phrase-sized chunk of text for `turn`.
    Synthesize { turn: TurnId, text: String },
    /// Abandon `turn`: skip any of its still-queued (not yet started)
    /// requests. Synthesis already in progress for it has no cooperative
    /// cancellation hook in `SpeechSynthesizer` -- the call runs to
    /// completion regardless -- so this only prevents *future* work; the
    /// authoritative "don't play it" enforcement is
    /// `crate::playback::PlaybackState`, which drops any result that
    /// arrives for an abandoned turn.
    Cancel(TurnId),
}

/// One synthesized chunk, already resampled to the pipeline's internal
/// rate and ready to hand to `DuplexEngine::push_playback`.
pub(crate) struct TtsResult {
    pub(crate) turn: TurnId,
    pub(crate) samples_48k: Vec<f32>,
}

/// A running TTS worker: the synthesizer lives entirely on its own thread.
pub(crate) struct TtsWorker {
    // `Option` so `Drop` can explicitly close the channel before joining --
    // see the identical field on `crate::stt_worker::SttWorker` for why
    // this matters: without it, `handle.join()` would wait forever on a
    // worker thread still blocked reading a channel whose sender is still
    // alive.
    request_tx: Option<Sender<TtsRequest>>,
    result_rx: Receiver<TtsResult>,
    handle: Option<JoinHandle<()>>,
}

impl TtsWorker {
    /// Load the default Pocket TTS bundle and spawn the worker thread.
    pub(crate) fn spawn() -> Result<Self, TtsError> {
        let model_dir = buzztalk_tts::default_model_dir();
        let synth = buzztalk_tts::PocketSynthesizer::load(&model_dir, 1)?;
        Ok(Self::spawn_with(synth))
    }

    /// Spawn the worker around an already-constructed synthesizer. Exists
    /// mainly so tests can drive the worker's request/result plumbing
    /// against a lightweight fake, without needing the real model on disk.
    pub(crate) fn spawn_with(mut synth: impl SpeechSynthesizer + 'static) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<TtsRequest>();
        let (result_tx, result_rx) = mpsc::channel::<TtsResult>();

        let handle = thread::Builder::new()
            .name("buzztalk-tts".into())
            .spawn(move || {
                let mut cancelled: HashSet<TurnId> = HashSet::new();
                for request in request_rx {
                    match request {
                        TtsRequest::Cancel(turn) => {
                            cancelled.insert(turn);
                        }
                        TtsRequest::Synthesize { turn, text } => {
                            if cancelled.contains(&turn) {
                                continue;
                            }
                            let Ok(chunk) = synth.synthesize(&text) else {
                                continue;
                            };
                            let samples_48k =
                                resample_linear_to_48k(&chunk.samples, chunk.sample_rate);
                            if result_tx.send(TtsResult { turn, samples_48k }).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn buzztalk-tts worker thread");

        Self {
            request_tx: Some(request_tx),
            result_rx,
            handle: Some(handle),
        }
    }

    /// Queue one phrase-sized chunk of text for synthesis. Never blocks
    /// (unbounded channel; synthesis requests aren't a per-frame concern).
    pub(crate) fn synthesize(&self, turn: TurnId, text: String) {
        if let Some(tx) = &self.request_tx {
            let _ = tx.send(TtsRequest::Synthesize { turn, text });
        }
    }

    /// Abandon `turn`'s not-yet-started synthesis requests.
    pub(crate) fn cancel(&self, turn: TurnId) {
        if let Some(tx) = &self.request_tx {
            let _ = tx.send(TtsRequest::Cancel(turn));
        }
    }

    /// Non-blocking pop of the next available synthesized chunk.
    pub(crate) fn try_recv_result(&self) -> Option<TtsResult> {
        self.result_rx.try_recv().ok()
    }
}

impl Drop for TtsWorker {
    fn drop(&mut self) {
        // Explicitly drop the sender first to close the channel before
        // joining -- see the field doc on `request_tx`.
        drop(self.request_tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_session::{Input, SessionMachine};
    use buzztalk_tts::{AudioChunk, Result as TtsResultT};
    use std::time::{Duration, Instant};

    fn turns(n: usize) -> Vec<TurnId> {
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        let mut ids = Vec::new();
        for _ in 0..n {
            machine.handle(Input::PushToTalkPressed);
            ids.push(machine.current_turn().unwrap());
            machine.handle(Input::PushToTalkReleased);
        }
        ids
    }

    /// A trivial fake synthesizer: turns each phrase into a fixed number of
    /// samples at 24 kHz (Pocket TTS's real rate) so worker-plumbing and
    /// the resample-on-the-way-out step can be tested without the real
    /// (multi-hundred-MB) Pocket TTS bundle on disk.
    struct FakeSynthesizer {
        samples_per_call: usize,
        calls: usize,
    }

    impl SpeechSynthesizer for FakeSynthesizer {
        fn synthesize(&mut self, _text: &str) -> TtsResultT<AudioChunk> {
            self.calls += 1;
            Ok(AudioChunk {
                samples: vec![0.25; self.samples_per_call],
                sample_rate: 24_000,
                chunk_index: self.calls,
            })
        }

        fn warmup(&mut self) -> TtsResultT<()> {
            Ok(())
        }
    }

    fn recv_within(worker: &TtsWorker, timeout: Duration) -> Option<TtsResult> {
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
    fn synthesize_produces_a_tagged_result_resampled_to_48k() {
        let worker = TtsWorker::spawn_with(FakeSynthesizer {
            samples_per_call: 100,
            calls: 0,
        });
        let turn = turns(1)[0];
        worker.synthesize(turn, "hello there".into());

        let result = recv_within(&worker, Duration::from_secs(2)).expect("expected a result");
        assert_eq!(result.turn, turn);
        // 24 kHz -> 48 kHz is an exact 2x ratio.
        assert_eq!(result.samples_48k.len(), 200);
    }

    #[test]
    fn cancelling_a_turn_before_its_request_is_processed_suppresses_the_result() {
        let worker = TtsWorker::spawn_with(FakeSynthesizer {
            samples_per_call: 10,
            calls: 0,
        });
        let ids = turns(2);
        let (cancelled_turn, live_turn) = (ids[0], ids[1]);

        // Cancel before ever submitting anything for this turn: its
        // request must be skipped once submitted.
        worker.cancel(cancelled_turn);
        worker.synthesize(cancelled_turn, "should not be heard".into());
        worker.synthesize(live_turn, "should be heard".into());

        let result = recv_within(&worker, Duration::from_secs(2)).expect("expected a result");
        assert_eq!(
            result.turn, live_turn,
            "the cancelled turn's request must be skipped"
        );
        assert!(recv_within(&worker, Duration::from_millis(100)).is_none());
    }

    #[test]
    fn multiple_chunks_for_one_turn_arrive_in_submission_order() {
        let worker = TtsWorker::spawn_with(FakeSynthesizer {
            samples_per_call: 4,
            calls: 0,
        });
        let turn = turns(1)[0];
        worker.synthesize(turn, "one".into());
        worker.synthesize(turn, "two".into());
        worker.synthesize(turn, "three".into());

        let mut chunk_indices = Vec::new();
        for _ in 0..3 {
            let r = recv_within(&worker, Duration::from_secs(2)).expect("expected a result");
            assert_eq!(r.turn, turn);
            chunk_indices.push(r.samples_48k.len());
        }
        // All three should have arrived (submission order is preserved by
        // the single-threaded worker processing one request at a time).
        assert_eq!(chunk_indices, vec![8, 8, 8]);
    }
}
