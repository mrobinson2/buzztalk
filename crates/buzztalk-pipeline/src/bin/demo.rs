//! `buzztalk-demo`: a terminal-driven conversation loop exercising
//! [`ConversationPipeline`] end to end -- capture, AEC, both VAD detectors,
//! the session machine, [`EchoAgent`], TTS, and playback, with barge-in.
//!
//! ```text
//! buzztalk-demo [--headphones] [--simulate [PATH]] [--seconds N]
//! ```
//!
//! * `--headphones` forces the barge-in gate to treat output as
//!   `OutputRoute::Headphones` (no acoustic loop, ERLE gate relaxed) --
//!   useful on a machine whose real output route can't be detected.
//! * `--simulate [PATH]` replaces live microphone capture with audio
//!   decoded from a WAV file, paced to real time as if it were the
//!   microphone. Defaults to the Parakeet test fixture shipped with the
//!   model bundle if no path is given. This exists because this
//!   development machine's audio input is a virtual device that captures
//!   digital silence -- there is nothing to transcribe from a real
//!   microphone here.
//! * `--seconds N` (default 30) is how long the demo runs before ending
//!   the session and exiting.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use buzztalk_pipeline::{
    ConversationPipeline, EchoAgent, OutputRoute, PipelineConfig, PipelineEvent,
};

fn main() {
    let mut headphones = false;
    let mut simulate: Option<PathBuf> = None;
    let mut seconds: u64 = 30;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headphones" => headphones = true,
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
    println!();

    let config = PipelineConfig {
        forced_output_route: headphones.then_some(OutputRoute::Headphones),
        simulate_capture: simulate,
        agent: Box::new(EchoAgent::new()),
        ..Default::default()
    };

    let pipeline = match ConversationPipeline::start(config) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("failed to start pipeline: {err}");
            std::process::exit(1);
        }
    };

    pipeline.start_session();

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_partial = String::new();
    while Instant::now() < deadline {
        match pipeline.recv_event_timeout(Duration::from_millis(200)) {
            Some(PipelineEvent::StateChanged(state)) => println!("[state]   {state:?}"),
            Some(PipelineEvent::Partial(text)) => {
                if text != last_partial {
                    println!("[partial] {text}");
                    last_partial = text;
                }
            }
            Some(PipelineEvent::FinalTranscript(text)) => println!("[final]   {text}"),
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
            Some(PipelineEvent::SessionEnded) => {
                println!("[state]   session ended");
                break;
            }
            _ => {}
        }
    }
}

fn print_usage() {
    println!("usage: buzztalk-demo [--headphones] [--simulate [PATH]] [--seconds N]");
}
