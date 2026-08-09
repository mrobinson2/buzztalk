//! Drop-in Tauri command module: the mic button's backend.
//!
//! Copy this into Buzz's `desktop/src-tauri/src/commands/` and register the
//! three commands in the Tauri builder (see DESKTOP-INTEGRATION.md). It
//! wraps `buzztalk-pipeline` + `buzztalk-buzz` — the same engine `buzztalkd`
//! runs — behind three commands the web frontend calls, and streams
//! pipeline events to the UI as a Tauri event named `buzztalk://event`.
//!
//! Add to `desktop/src-tauri/Cargo.toml`:
//! ```toml
//! buzztalk-pipeline = { git = "https://github.com/mrobinson2/buzztalk" }
//! buzztalk-buzz      = { git = "https://github.com/mrobinson2/buzztalk" }
//! buzztalk-session   = { git = "https://github.com/mrobinson2/buzztalk" }
//! ```

use std::sync::Mutex;
use std::time::Duration;

use buzztalk_buzz::{BuzzAgent, BuzzConfig, KeySource, PublicKey, Uuid};
use buzztalk_pipeline::{ConversationPipeline, PipelineConfig, PipelineEvent};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Held in Tauri's managed state. `None` when the mic is off.
#[derive(Default)]
pub struct VoiceState(pub Mutex<Option<ConversationPipeline>>);

/// One event pushed to the frontend as `buzztalk://event`. Mirrors the
/// pipeline's `PipelineEvent`s the UI cares about.
#[derive(Serialize, Clone)]
#[serde(tag = "kind", content = "text")]
enum UiEvent {
    Partial(String),
    Final(String),
    Agent(String),
    State(String),
    AudioRebuilt(String),
    Lost(String),
}

/// Start voice for a channel. `key_file` holds the logged-in user's nsec
/// (Buzz already has the key in its keyring — write it to an app-private
/// temp file and pass the path, or adapt `KeySource` to take the key
/// in-memory). Voice messages publish as the user; `agent_pubkey`'s replies
/// are spoken.
#[tauri::command]
pub async fn buzztalk_start(
    app: AppHandle,
    state: State<'_, VoiceState>,
    relay_url: String,
    channel_uuid: String,
    agent_pubkey: String,
    key_file: String,
    // macOS: true (VoiceProcessingIO, survives Bluetooth). Windows/Linux:
    // false (portable cpal engine) — the frontend passes cfg!(macos).
    use_voice_processing: bool,
) -> Result<(), String> {
    let channel = Uuid::parse_str(&channel_uuid).map_err(|e| e.to_string())?;
    let agent = PublicKey::parse(&agent_pubkey).map_err(|e| e.to_string())?;

    let buzz = BuzzConfig::new(relay_url, channel, KeySource::File(key_file.into()))
        .with_agent_pubkeys(vec![agent]);
    let backend = BuzzAgent::connect(buzz).map_err(|e| e.to_string())?;

    let mut pipeline = ConversationPipeline::start(PipelineConfig {
        agent: Box::new(backend),
        use_voice_processing,
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    pipeline.start_session();

    // Pump events to the frontend on a background task. `take_event_rx()`
    // is the one small addition to expose on `ConversationPipeline` — it
    // hands out the `Receiver<PipelineEvent>` the pump owns, so the pump
    // isn't contending with start/stop on the state mutex. See
    // DESKTOP-INTEGRATION.md; until it exists, poll `recv_event_timeout`
    // from a task that holds the pipeline.
    let app2 = app.clone();
    let events = pipeline.take_event_rx().expect("event rx");
    std::thread::spawn(move || {
        while let Ok(ev) = events.recv_timeout(Duration::from_millis(200)) {
            let ui = match ev {
                PipelineEvent::Partial(t) => UiEvent::Partial(t),
                PipelineEvent::FinalTranscript(t) => UiEvent::Final(t),
                PipelineEvent::AgentText(t) => UiEvent::Agent(t),
                PipelineEvent::StateChanged(s) => UiEvent::State(format!("{s:?}")),
                PipelineEvent::AudioDeviceRebuilt { reason } => UiEvent::AudioRebuilt(reason),
                PipelineEvent::CapabilityLost { what, reason } => {
                    UiEvent::Lost(format!("{what}: {reason}"))
                }
                _ => continue,
            };
            let _ = app2.emit("buzztalk://event", ui);
        }
    });

    *state.0.lock().unwrap() = Some(pipeline);
    Ok(())
}

/// Stop voice. Dropping the pipeline joins its threads.
#[tauri::command]
pub fn buzztalk_stop(state: State<'_, VoiceState>) {
    if let Some(p) = state.0.lock().unwrap().take() {
        p.end_session();
    }
}

/// Whether voice is currently running (for the button's toggle state on
/// window focus / reload).
#[tauri::command]
pub fn buzztalk_is_active(state: State<'_, VoiceState>) -> bool {
    state.0.lock().unwrap().is_some()
}
