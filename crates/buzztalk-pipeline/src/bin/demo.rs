//! `buzztalk-demo`: a terminal-driven conversation loop exercising
//! [`ConversationPipeline`] end to end -- capture, AEC, both VAD detectors,
//! the session machine, [`EchoAgent`], TTS, and playback, with barge-in.
//!
//! ```text
//! buzztalk-demo [--headphones] [--no-aec] [--vpio] [--simulate [PATH]] [--seconds N]
//! buzztalk-demo --download-models
//! buzztalk-demo --model-status
//! ```
//!
//! * `--download-models` fetches and verifies both speech model bundles
//!   (via `buzztalk-models`) into `~/.buzztalk/models` (or
//!   `BUZZTALK_MODELS_DIR`), then exits -- this is the fix for "the
//!   published binary doesn't work on a fresh machine because the models
//!   have to be `curl`ed by hand." Safe to re-run: already-correct files
//!   are left alone.
//! * `--model-status` reports what's present, what's missing, and how much
//!   disk space the models are using, then exits.
//! * `--headphones` forces the barge-in gate to treat output as
//!   `OutputRoute::Headphones` (no acoustic loop, ERLE gate relaxed) --
//!   useful on a machine whose real output route can't be detected.
//! * `--no-aec` runs with the null echo canceller instead of the best
//!   compiled-in backend -- for telling whether AEC itself is why barge-in
//!   isn't working. See the loud warning this prints at startup: with no
//!   real echo suppression to measure, the barge-in gate has nothing to
//!   check before trusting a candidate, so it becomes far more
//!   trigger-happy on ordinary playback bleed.
//! * `--vpio` (macOS only) uses `VoiceProcessingIO` instead of `DuplexEngine`'s
//!   two independent `cpal` streams -- fixes a Bluetooth headset's
//!   microphone being starved to digital silence by two uncoordinated
//!   CoreAudio clients. See `PipelineConfig::use_voice_processing`'s docs.
//! * `--simulate [PATH]` replaces live microphone capture with audio
//!   decoded from a WAV file, paced to real time as if it were the
//!   microphone. Defaults to the Parakeet test fixture shipped with the
//!   model bundle if no path is given. This exists because this
//!   development machine's audio input is a virtual device that captures
//!   digital silence -- there is nothing to transcribe from a real
//!   microphone here.
//! * `--seconds N` (default 30) is how long the demo runs before ending
//!   the session and exiting.
//!
//! Per the project's design rule ("every failure degrades to something
//! usable"), a missing STT or TTS model does not stop this demo from
//! starting -- it prints the pipeline's capability report right after
//! startup so a half-finished model download shows up as "degraded but
//! working," not as a silent dead feature. Only a real fatal error (no
//! audio device, or a bad `--simulate` WAV) exits non-zero.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use buzztalk_pipeline::{
    ConversationPipeline, EchoAgent, OutputRoute, PipelineConfig, PipelineEvent,
};

fn main() {
    let mut headphones = false;
    let mut no_aec = false;
    let mut vpio = false;
    let mut simulate: Option<PathBuf> = None;
    let mut seconds: u64 = 30;
    let mut download_models = false;
    let mut model_status = false;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headphones" => headphones = true,
            "--no-aec" => no_aec = true,
            "--vpio" => vpio = true,
            "--simulate" => {
                let path = match args.peek() {
                    Some(p) if !p.starts_with("--") => PathBuf::from(args.next().unwrap()),
                    _ => buzztalk_pipeline::default_simulate_wav(),
                };
                simulate = Some(path);
            }
            "--seconds" => {
                seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(seconds);
            }
            "--download-models" => download_models = true,
            "--model-status" => model_status = true,
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("buzztalk-demo: unrecognized argument {other:?}");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    if model_status {
        print!("{}", buzztalk_models::status());
        return;
    }
    if download_models {
        match buzztalk_models::ensure_models(buzztalk_models::ModelSet::All) {
            Ok(()) => {
                println!("buzztalk-demo: model download complete.");
            }
            Err(err) => {
                eprintln!("buzztalk-demo: model download failed: {err}");
                std::process::exit(1);
            }
        }
        return;
    }

    println!("buzztalk-demo starting");
    if let Some(path) = &simulate {
        println!(
            "  --simulate: feeding {} as microphone input \
             (this machine's real mic input is digital silence)",
            path.display()
        );
    }
    if headphones {
        println!("  --headphones: forcing output route to headphones");
    }
    if no_aec {
        println!(
            "  --no-aec: running with NO echo cancellation (null canceller). \
             BARGE-IN WILL BE UNRELIABLE in this mode -- with no real echo \
             suppression to measure, the barge-in gate has nothing to check \
             before trusting a candidate, so ordinary playback bleed can \
             falsely trigger it. Use this only to test whether AEC itself \
             is the thing breaking your setup, not for normal use."
        );
    }
    if vpio {
        println!(
            "  --vpio: using macOS VoiceProcessingIO for capture+render \
             (fixes Bluetooth headsets starving the microphone to silence \
             under two independent cpal streams; macOS only)"
        );
    }
    println!();

    let config = PipelineConfig {
        forced_output_route: headphones.then_some(OutputRoute::Headphones),
        simulate_capture: simulate,
        agent: Box::new(EchoAgent::new()),
        no_aec,
        use_voice_processing: vpio,
        ..Default::default()
    };

    let pipeline = match ConversationPipeline::start(config) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("failed to start pipeline: {err}");
            std::process::exit(1);
        }
    };

    // Every failure degrades to something usable: STT/TTS each loading is
    // best-effort, not fatal to `start` -- print what we actually got
    // before pumping events, so a missing model is visibly "degraded but
    // running," not silence.
    println!("capabilities: {}", pipeline.capabilities().report());
    if !pipeline.capabilities().stt || !pipeline.capabilities().tts {
        println!(
            "  hint: missing speech models? run `buzztalk-demo --download-models` to fetch \
             them (or `buzztalk-demo --model-status` to see what's there)."
        );
    }
    println!();

    pipeline.start_session();

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_partial = String::new();
    while Instant::now() < deadline {
        match pipeline.recv_event_timeout(Duration::from_millis(200)) {
            Some(PipelineEvent::Capabilities(caps)) => {
                println!("[caps]    {}", caps.report())
            }
            Some(PipelineEvent::CapabilityLost { what, reason }) => {
                println!("[lost]    {what}: {reason}")
            }
            Some(PipelineEvent::AudioDeviceRebuilt { reason }) => {
                println!("[audio]   engine rebuilt: {reason}")
            }
            Some(PipelineEvent::StateChanged(state)) => println!("[state]   {state:?}"),
            Some(PipelineEvent::Partial(text)) => {
                if text != last_partial {
                    println!("[partial] {text}");
                    last_partial = text;
                }
            }
            Some(PipelineEvent::FinalTranscript(text)) => println!("[final]   {text}"),
            Some(PipelineEvent::FragmentFiltered { text, duration_ms }) => {
                println!(
                    "[filter]  dropped micro-utterance fragment (\"{text}\", {duration_ms} ms)"
                )
            }
            Some(PipelineEvent::AgentText(text)) => println!("[agent]   {text}"),
            Some(PipelineEvent::TurnMetrics(summary)) => println!("[metrics] {summary}"),
            Some(PipelineEvent::Dropped { what, total }) => {
                println!("[drop]    {what}: {total} total")
            }
            Some(PipelineEvent::SessionEnded) => {
                println!("[state]   session ended");
                break;
            }
            None => {}
        }
    }

    println!("\nbuzztalk-demo stopping (--seconds elapsed)");
    pipeline.end_session();
    // Give the orchestrator a moment to flush a final state/metrics event
    // before the process exits and drops the pipeline.
    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match pipeline.recv_event_timeout(Duration::from_millis(100)) {
            Some(PipelineEvent::TurnMetrics(summary)) => println!("[metrics] {summary}"),
            Some(PipelineEvent::StateChanged(state)) => println!("[state]   {state:?}"),
            Some(PipelineEvent::CapabilityLost { what, reason }) => {
                println!("[lost]    {what}: {reason}")
            }
            Some(PipelineEvent::SessionEnded) => {
                println!("[state]   session ended");
                break;
            }
            _ => {}
        }
    }
}

fn print_usage() {
    println!(
        "usage: buzztalk-demo [--headphones] [--no-aec] [--vpio] [--simulate [PATH]] [--seconds N]\n\
         \x20         [--download-models] [--model-status]"
    );
}
