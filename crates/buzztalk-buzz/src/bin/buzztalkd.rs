//! `buzztalkd`: headless BuzzTalk <-> Buzz bridge.
//!
//! Runs the full `ConversationPipeline` from `buzztalk-pipeline` with
//! [`BuzzAgent`] in place of the `EchoAgent` the desktop demo uses -- this
//! is the artifact that proves voice-to-Buzz works without the Tauri
//! desktop app.
//!
//! ```text
//! buzztalkd --relay <wss://...> --channel <uuid>
//!           --agent-pubkey <hex-or-npub> [--agent-pubkey <hex-or-npub> ...]
//!           [--key-env VAR | --key-file PATH]
//!           [--headphones] [--no-aec] [--simulate [PATH]] [--seconds N]
//!           [--quiet-period-ms N]
//! buzztalkd --download-models
//! buzztalkd --model-status
//! ```
//!
//! `--headphones`, `--no-aec`, `--simulate [PATH]`, and `--seconds N` mirror
//! `buzztalk-demo` (`buzztalk-pipeline/src/bin/demo.rs`) exactly, so the two
//! binaries are interchangeable for manual testing. `--seconds` defaults to
//! `0`, meaning "run until the process is killed" -- unlike the demo, this
//! binary is meant to run unattended. Per the pipeline's design rule
//! ("every failure degrades to something usable"), a missing STT or TTS
//! model does not stop `buzztalkd` from starting -- it prints the
//! pipeline's capability report right after startup, same as the demo.
//!
//! `--download-models` and `--model-status` also mirror the demo exactly
//! (both delegate to `buzztalk-models`): checked before `--relay`/
//! `--channel`/`--agent-pubkey` are required, so `buzztalkd --model-status`
//! works standalone without a relay configured.
//!
//! The signing key is never accepted as a flag (it would end up in argv and
//! shell history). If neither `--key-env` nor `--key-file` is given,
//! `buzztalkd` reads it from the `BUZZTALK_NOSTR_SECRET` environment
//! variable.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use buzztalk_buzz::{BuzzAgent, BuzzConfig, KeySource, PublicKey, Uuid};
use buzztalk_pipeline::{ConversationPipeline, OutputRoute, PipelineConfig, PipelineEvent};
use buzztalk_session::SessionState;

struct Args {
    relay: String,
    channel: Uuid,
    agent_pubkeys: Vec<PublicKey>,
    key_source: KeySource,
    quiet_period: Option<Duration>,
    endpoint_silence_ms: Option<u64>,
    output_device: Option<String>,
    headphones: bool,
    no_aec: bool,
    simulate: Option<PathBuf>,
    seconds: u64,
}

impl Args {
    fn parse<I: Iterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut relay = None;
        let mut channel = None;
        let mut agent_pubkeys = Vec::new();
        let mut key_env = None;
        let mut key_file = None;
        let mut quiet_period = None;
        let mut endpoint_silence_ms = None;
        let mut output_device = None;
        let mut headphones = false;
        let mut no_aec = false;
        let mut simulate = None;
        let mut seconds = 0u64;

        let mut it = args.peekable();

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--relay" => relay = Some(require_value(&mut it, "--relay")?),
                "--channel" => {
                    let raw = require_value(&mut it, "--channel")?;
                    channel = Some(
                        Uuid::parse_str(&raw)
                            .map_err(|e| format!("--channel {raw:?} is not a valid UUID: {e}"))?,
                    );
                }
                "--agent-pubkey" => {
                    let raw = require_value(&mut it, "--agent-pubkey")?;
                    let pk = PublicKey::parse(&raw).map_err(|e| {
                        format!("--agent-pubkey {raw:?} is not a valid pubkey: {e}")
                    })?;
                    agent_pubkeys.push(pk);
                }
                "--key-env" => key_env = Some(require_value(&mut it, "--key-env")?),
                "--key-file" => {
                    key_file = Some(PathBuf::from(require_value(&mut it, "--key-file")?))
                }
                "--quiet-period-ms" => {
                    let raw = require_value(&mut it, "--quiet-period-ms")?;
                    let ms: u64 = raw
                        .parse()
                        .map_err(|_| format!("--quiet-period-ms {raw:?} is not a number"))?;
                    quiet_period = Some(Duration::from_millis(ms));
                }
                "--endpoint-silence-ms" => {
                    let raw = require_value(&mut it, "--endpoint-silence-ms")?;
                    let ms: u64 = raw
                        .parse()
                        .map_err(|_| format!("--endpoint-silence-ms {raw:?} is not a number"))?;
                    endpoint_silence_ms = Some(ms);
                }
                "--output-device" => {
                    output_device = Some(require_value(&mut it, "--output-device")?);
                }
                "--headphones" => headphones = true,
                "--no-aec" => no_aec = true,
                "--simulate" => {
                    let path = match it.peek() {
                        Some(p) if !p.starts_with("--") => PathBuf::from(it.next().unwrap()),
                        _ => buzztalk_pipeline::default_simulate_wav(),
                    };
                    simulate = Some(path);
                }
                "--seconds" => {
                    let raw = require_value(&mut it, "--seconds")?;
                    seconds = raw
                        .parse()
                        .map_err(|_| format!("--seconds {raw:?} is not a number"))?;
                }
                other => return Err(format!("unrecognized argument {other:?}")),
            }
        }

        if key_env.is_some() && key_file.is_some() {
            return Err("--key-env and --key-file are mutually exclusive".to_string());
        }
        let key_source = match (key_env, key_file) {
            (Some(var), None) => KeySource::EnvVar(var),
            (None, Some(path)) => KeySource::File(path),
            (None, None) => KeySource::default_env(),
            (Some(_), Some(_)) => unreachable!("checked above"),
        };

        Ok(Args {
            relay: relay.ok_or("--relay is required")?,
            channel: channel.ok_or("--channel is required")?,
            agent_pubkeys,
            key_source,
            quiet_period,
            endpoint_silence_ms,
            output_device,
            headphones,
            no_aec,
            simulate,
            seconds,
        })
    }
}

fn require_value<I: Iterator<Item = String>>(
    it: &mut std::iter::Peekable<I>,
    flag: &str,
) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage() {
    eprintln!(
        "usage: buzztalkd --relay <wss://...> --channel <uuid> \\\n\
         \x20         --agent-pubkey <hex-or-npub> [--agent-pubkey ...] \\\n\
         \x20         [--key-env VAR | --key-file PATH] \\\n\
         \x20         [--headphones] [--no-aec] [--simulate [PATH]] [--seconds N] \\\n\
         \x20         [--quiet-period-ms N]\n\
         \x20      buzztalkd --download-models\n\
         \x20      buzztalkd --model-status\n\n\
         The signing key defaults to the BUZZTALK_NOSTR_SECRET environment variable\n\
         when neither --key-env nor --key-file is given."
    );
}

fn main() {
    // Checked ahead of `Args::parse` (and its `--relay`/`--channel`/
    // `--agent-pubkey` requirements) so these two work standalone --
    // fetching or inspecting models has nothing to do with having a relay
    // configured yet.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.iter().any(|a| a == "--model-status") {
        print!("{}", buzztalk_models::status());
        return;
    }
    if raw_args.iter().any(|a| a == "--download-models") {
        match buzztalk_models::ensure_models(buzztalk_models::ModelSet::All) {
            Ok(()) => println!("buzztalkd: model download complete."),
            Err(err) => {
                eprintln!("buzztalkd: model download failed: {err}");
                std::process::exit(1);
            }
        }
        return;
    }

    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("buzztalkd: {e}");
            print_usage();
            std::process::exit(2);
        }
    };

    if args.agent_pubkeys.is_empty() {
        eprintln!(
            "buzztalkd: warning: no --agent-pubkey configured; every inbound \
             reply will be rejected as an unknown author"
        );
    }

    let mut buzz_config = BuzzConfig::new(args.relay.clone(), args.channel, args.key_source)
        .with_agent_pubkeys(args.agent_pubkeys);
    if let Some(quiet_period) = args.quiet_period {
        buzz_config.reply_quiet_period = quiet_period;
    }

    println!("buzztalkd starting");
    println!("  relay:   {}", args.relay);
    println!("  channel: {}", args.channel);
    if let Some(path) = &args.simulate {
        println!(
            "  --simulate: feeding {} as microphone input",
            path.display()
        );
    }
    if args.headphones {
        println!("  --headphones: forcing output route to headphones");
    }
    if args.no_aec {
        println!(
            "  --no-aec: running with NO echo cancellation (null canceller). \
             BARGE-IN WILL BE UNRELIABLE in this mode -- use only to test \
             whether AEC itself is the thing breaking your setup."
        );
    }
    println!();

    let agent = match BuzzAgent::connect(buzz_config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("buzztalkd: failed to start Buzz connection: {e}");
            std::process::exit(1);
        }
    };

    let pipeline_config = PipelineConfig {
        duplex: buzztalk_pipeline::DuplexConfig {
            output_device: args.output_device.clone(),
            ..Default::default()
        },
        forced_output_route: args.headphones.then_some(OutputRoute::Headphones),
        simulate_capture: args.simulate,
        agent: Box::new(agent),
        no_aec: args.no_aec,
        endpoint_silence_ms: args.endpoint_silence_ms,
        ..Default::default()
    };

    let pipeline = match ConversationPipeline::start(pipeline_config) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("buzztalkd: failed to start pipeline: {err}");
            std::process::exit(1);
        }
    };

    // Every failure degrades to something usable: STT/TTS each loading is
    // best-effort, not fatal to `start` -- print what we actually got
    // before pumping events, so a missing model is visibly "degraded but
    // running," not silence (same rule `buzztalk-demo` follows).
    println!("capabilities: {}", pipeline.capabilities().report());
    if !pipeline.capabilities().stt || !pipeline.capabilities().tts {
        println!(
            "  hint: missing speech models? run `buzztalkd --download-models` to fetch \
             them (or `buzztalkd --model-status` to see what's there)."
        );
    }
    println!();

    pipeline.start_session();

    let deadline = (args.seconds > 0).then(|| Instant::now() + Duration::from_secs(args.seconds));
    let mut last_partial = String::new();
    loop {
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }
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
            Some(PipelineEvent::StateChanged(state)) => {
                println!("[state]   {state:?}");
                // The session machine ends the session after IDLE_TIMEOUT
                // (90 s) of silence -- right for the interactive demo, wrong
                // for a daemon meant to run unattended: one quiet minute and
                // a half would leave the process alive but deaf forever.
                // Re-arm immediately so Listening is the steady state.
                if state == SessionState::Idle {
                    println!("[state]   idle timeout -- restarting session");
                    pipeline.start_session();
                }
            }
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

    println!("\nbuzztalkd stopping");
    pipeline.end_session();
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
