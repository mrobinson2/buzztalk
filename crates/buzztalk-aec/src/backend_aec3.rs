//! Backend over the `aec3` crate (pure-Rust port of WebRTC's AEC3).
//!
//! `aec3` exposes a full render/capture pipeline (high-pass filter -> AEC3 ->
//! noise suppression -> AGC2) driven by 10 ms frames, which lines up exactly
//! with [`buzztalk_core::FRAME_SAMPLES`] at [`buzztalk_core::SAMPLE_RATE_HZ`].
//! Metrics (including ERLE) are read straight off the AEC3 node -- no
//! do-it-yourself energy accounting needed here.
//!
//! # Why this is a thread, not a direct wrapper
//!
//! `aec3::pipelines::linear::LinearPipeline` owns a graph `Runtime` that is
//! explicitly single-threaded internally (its packets are `Rc`-counted), so
//! `LinearPipeline` is not `Send`. [`EchoCanceller`] requires `Send`. That
//! combination was only discovered by actually wiring the crate into our
//! trait -- `cargo check --features backend-aec3` on the bare dependency
//! compiles fine; it is *this* crate's `impl EchoCanceller for Aec3Canceller`
//! that fails to compile without the indirection below.
//!
//! The fix: confine the pipeline to one dedicated worker thread and talk to
//! it over a pair of `std::sync::mpsc` channels. [`Aec3Canceller`] itself
//! holds only a `Sender`, a `Receiver`, and a `JoinHandle`, all of which are
//! `Send`. Every call blocks on a round trip to the worker, so behaviour is
//! synchronous from the caller's point of view -- just with one thread hop of
//! overhead per frame.

use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear;
use buzztalk_core::{AecStats, EchoCanceller, Error, Result, FRAME_SAMPLES, SAMPLE_RATE_HZ};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

enum Command {
    Render(Vec<f32>),
    Capture(Vec<f32>),
    SetDelay(u32),
    Stats,
}

enum Response {
    Ack,
    Frame(Vec<f32>),
    Stats(AecStats),
    Error(String),
}

/// [`EchoCanceller`] backed by the `aec3` crate's standard linear pipeline,
/// running on a dedicated worker thread (see module docs for why).
pub struct Aec3Canceller {
    cmd_tx: Sender<Command>,
    resp_rx: Receiver<Response>,
    worker: Option<JoinHandle<()>>,
}

impl Aec3Canceller {
    /// Builds a new canceller configured for one mono 10 ms frame at
    /// [`SAMPLE_RATE_HZ`].
    pub fn new() -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (resp_tx, resp_rx) = mpsc::channel::<Response>();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();

        let worker = thread::Builder::new()
            .name("buzztalk-aec3".into())
            .spawn(move || {
                let format = AudioFormat::ten_ms(SAMPLE_RATE_HZ, 1);
                debug_assert_eq!(format.sample_count(), FRAME_SAMPLES);
                // Noise suppression and AGC2 are deliberately disabled: this
                // crate's `EchoCanceller` trait is scoped to echo
                // cancellation, and AGC2 in particular actively re-normalizes
                // output loudness, which masks (in raw signal energy terms)
                // exactly the reduction AEC3 achieved. Full-chain APM
                // (NS/AGC) belongs in a separate stage, not baked into an AEC
                // backend.
                let mut pipeline = match linear::builder(format, format)
                    .enable_noise_suppression(false)
                    .enable_gain_controller2(false)
                    .export_metrics(true)
                    .build()
                {
                    Ok(pipeline) => {
                        if ready_tx.send(Ok(())).is_err() {
                            return;
                        }
                        pipeline
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("aec3: failed to build pipeline: {e}")));
                        return;
                    }
                };

                let mut last_erle_db: Option<f32> = None;
                let mut last_delay_ms: Option<u32> = None;

                while let Ok(cmd) = cmd_rx.recv() {
                    let response = match cmd {
                        Command::Render(frame) => match pipeline.handle_render_frame(&frame) {
                            Ok(()) => Response::Ack,
                            Err(e) => Response::Error(format!("aec3: render frame failed: {e}")),
                        },
                        Command::Capture(frame) => {
                            let mut output = vec![0.0f32; frame.len()];
                            match pipeline.process_capture_frame(&frame, &mut output) {
                                Ok(_) => {
                                    if let Ok(Some(packet)) = pipeline.try_pull_metrics() {
                                        let metrics = packet.payload();
                                        last_erle_db =
                                            Some(metrics.echo_return_loss_enhancement as f32);
                                        if metrics.delay_ms >= 0 {
                                            last_delay_ms = Some(metrics.delay_ms as u32);
                                        }
                                    }
                                    Response::Frame(output)
                                }
                                Err(e) => {
                                    Response::Error(format!("aec3: capture frame failed: {e}"))
                                }
                            }
                        }
                        Command::SetDelay(delay_ms) => {
                            let _ = pipeline.set_delay_ms(delay_ms as i32);
                            Response::Ack
                        }
                        Command::Stats => Response::Stats(AecStats {
                            erle_db: last_erle_db,
                            estimated_delay_ms: last_delay_ms,
                            double_talk: false,
                        }),
                    };
                    if resp_tx.send(response).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::Aec(format!("aec3: failed to spawn worker thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(Error::Aec(e)),
            Err(_) => {
                return Err(Error::Aec(
                    "aec3: worker thread exited before initialising".into(),
                ))
            }
        }

        Ok(Self {
            cmd_tx,
            resp_rx,
            worker: Some(worker),
        })
    }

    fn call(&self, cmd: Command) -> Result<Response> {
        self.cmd_tx
            .send(cmd)
            .map_err(|_| Error::Aec("aec3: worker thread is gone".into()))?;
        self.resp_rx
            .recv()
            .map_err(|_| Error::Aec("aec3: worker thread is gone".into()))
    }
}

impl EchoCanceller for Aec3Canceller {
    fn process_render(&mut self, far_end: &[f32]) -> Result<()> {
        buzztalk_core::check_frame(far_end)?;
        match self.call(Command::Render(far_end.to_vec()))? {
            Response::Ack => Ok(()),
            Response::Error(e) => Err(Error::Aec(e)),
            _ => Err(Error::Aec("aec3: unexpected worker response".into())),
        }
    }

    fn process_capture(&mut self, near_end: &mut [f32]) -> Result<()> {
        buzztalk_core::check_frame(near_end)?;
        match self.call(Command::Capture(near_end.to_vec()))? {
            Response::Frame(output) => {
                near_end.copy_from_slice(&output);
                Ok(())
            }
            Response::Error(e) => Err(Error::Aec(e)),
            _ => Err(Error::Aec("aec3: unexpected worker response".into())),
        }
    }

    fn set_stream_delay_ms(&mut self, delay_ms: u32) {
        let _ = self.call(Command::SetDelay(delay_ms));
    }

    fn stats(&self) -> AecStats {
        match self.call(Command::Stats) {
            Ok(Response::Stats(stats)) => stats,
            _ => AecStats::default(),
        }
    }

    fn name(&self) -> &'static str {
        "aec3"
    }
}

impl Drop for Aec3Canceller {
    fn drop(&mut self) {
        // Struct fields are only dropped *after* this method body returns, so
        // `self.cmd_tx` is still alive at this point and the worker's
        // `cmd_rx.recv()` would never unblock. Force-close the channel first
        // by swapping in a throwaway sender, then join.
        let (unused_tx, _unused_rx) = mpsc::channel::<Command>();
        drop(std::mem::replace(&mut self.cmd_tx, unused_tx));
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
