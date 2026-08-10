# Buzz Desktop BuzzTalk Audio Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a working, feature-gated Conversation button to Buzz Desktop that moves Huddle microphone capture and agent playback into BuzzTalk's native macOS VoiceProcessingIO duplex engine, while preserving Buzz's existing identity, relay, STT, TTS, models, agent routing, and fallback audio path.

**Architecture:** Buzz already owns the product semantics: Huddle lifecycle, the active channel, signed messages, agent membership, model downloads, transcription, synthesis, and cancellation. The bridge changes only audio ownership. When Conversation mode is on, a Rust-owned VoiceProcessingIO engine produces 48 kHz mono capture for the existing Huddle fan-out and accepts existing TTS PCM for playback; when it is off or fails, Buzz's current WebView `getUserMedia` + AudioWorklet capture and rodio output remain authoritative.

**Tech Stack:** Rust, Tauri 2, React 19, TypeScript, Tailwind CSS, `buzztalk-audio`, CoreAudio VoiceProcessingIO, existing Buzz Huddle STT/TTS and test infrastructure.

## Relationship to the Gateway Launcher

This bridge and the [BuzzTalk Gateway Launcher](../specs/2026-08-10-buzztalk-gateway-launcher-design.md) are complementary and independent. The launcher manages the standalone `buzztalkd` process for CLI/operator use. This bridge embeds only the `buzztalk-audio` VoiceProcessingIO engine in Buzz Desktop; it never launches `buzztalkd`, a sidecar, or a second relay or identity path.

Neither capability is a stepping-stone, fallback product, or temporary substitute for the other. The launcher's macOS/Windows process-management support does not imply a Windows Desktop Audio Bridge. The shared boundary and dependency decision is recorded in [Launcher and Desktop Bridge Boundaries](../decisions/2026-08-10-launcher-bridge-boundaries.md).

## Global Constraints

- Target upstream repository: `block/buzz`, baseline `main` commit `07a3c768d619db31fee3f0590f9433cdd1213e8f` (2026-08-10). Rebase and remap paths if upstream moves; preserve the interfaces and acceptance criteria in this plan.
- Draft experiment revision: `9fbbfc61260bb63c714a5c3694ec92cc8a602406`. A personal Git dependency is permitted only while the upstream PR remains draft. Task 0 must recommend a maintained dependency home before implementation; the PR must not merge until Buzz maintainers accept the owner and the manifest uses that maintained source.
- This bridge is supported only on Apple Silicon macOS and hidden behind runtime capability. Intel macOS, Windows, and Linux continue using the current Huddle path without a disabled control or empty layout slot. No future native bridge support is claimed.
- The user-facing name is **Conversation mode**. `BuzzTalk` is an implementation attribution in help text and developer documentation, not the primary control label.
- Buzz retains its existing identity and signing path. No nsec, key file, or signing secret crosses the frontend boundary or enters BuzzTalk code.
- Buzz retains its existing Huddle relay connection, STT/TTS implementations, model manager, voice selection, agent list, and message publication path. Do not launch `buzztalkd` and do not create a second relay session.
- At most one microphone producer is authoritative. WebView capture and native VoiceProcessingIO capture must never feed Huddle simultaneously.
- At most one playback owner is authoritative. Agent PCM must pass through VoiceProcessingIO while Conversation mode is active so playback and its render reference are identical.
- Failed bridge start restores the prior WebView audio path automatically. Failed bridge stop must leave the Huddle muted rather than accidentally transmitting.
- Leaving a Huddle, signing out, app shutdown, or identity recovery stops and joins every bridge worker.
- Bridge telemetry contains stable phases, durations, and error codes only. User-visible errors add fixed safe recovery copy. Neither contains audio, keys, transcripts, channel identifiers, relay URLs, pubkeys, device paths, or raw OS/dependency errors.
- No `unsafe` code in Buzz. No new production `unwrap()` or `expect()`. Every new public API has a doc comment.
- Activate Hermit before Buzz commands: `. ./bin/activate-hermit`.
- Commit each task independently with DCO sign-off: `git commit -s`.
- Run `just ci` before opening the PR. Root `cargo test` does not cover Desktop Tauri; always run `cargo test --manifest-path desktop/src-tauri/Cargo.toml` for native changes.

---

## Product and Design Handoff

### Product decision

Do not add a second microphone or duplicate Buzz's Huddle controls. Add one Conversation-mode toggle to the existing `HuddleBar` control cluster, immediately after `MicControls` and before `SpeakerControls`.

Conversation mode means:

1. Buzz stops the WebView AudioWorklet and releases its `MediaStreamTrack`.
2. The native bridge opens one VoiceProcessingIO unit for capture and playback.
3. Native capture feeds the same Huddle STT and audio-relay consumers as `push_audio_pcm` does today.
4. Existing TTS PCM is queued into the VoiceProcessingIO playback ring instead of a separate rodio device.
5. Existing Huddle speech detection and `tts_cancel` stop agent speech when the user interrupts.
6. Turning the mode off reverses the cutover and reacquires the prior WebView input device.

### Main HuddleBar layout

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ participants …   [ Mic ▴ ] [ 〰 Conversation ] [ Speaker ▴ ] [ Leave ]     │
└─────────────────────────────────────────────────────────────────────────────┘

Active:
┌─────────────────────────────────────────────────────────────────────────────┐
│ participants …   [ Mic ▴ ] [ ● Listening ]    [ Speaker ▴ ] [ Leave ]     │
└─────────────────────────────────────────────────────────────────────────────┘
```

In compact Huddle surfaces, render the same control as an icon-only `AudioWaveform` button with the full state in its tooltip and accessible name.

### Component contract

Create `desktop/src/features/huddle/components/AudioBridgeButton.tsx`.

```ts
export type AudioBridgePhase =
  | "off"
  | "starting"
  | "listening"
  | "thinking"
  | "speaking"
  | "stopping"
  | "error";

export interface AudioBridgeStatus {
  supported: boolean;
  enabled: boolean;
  phase: AudioBridgePhase;
  error_code: string | null;
  error_message: string | null;
}

export interface AudioBridgeButtonProps {
  compact?: boolean;
  status: AudioBridgeStatus;
  onToggle: () => Promise<void> | void;
}
```

### Visual specifications

- Reuse `@/shared/ui/button`, `Tooltip`, and existing Huddle control classes. Introduce no new color primitives.
- Main mode: `Button` with `size="sm"`, `variant="secondary"`, `h-8`, `gap-1.5`, `px-2.5` and `AudioWaveform` at `h-4 w-4`.
- Compact mode: match the existing Mic split-button height and width: `h-8 w-8`, icon-only, sidebar foreground and hover tokens.
- Inactive: existing secondary/ghost surface and muted foreground.
- Active: use existing primary semantic tokens, `bg-primary/15 text-primary`; retain a visible focus ring.
- Error: use existing destructive semantic tokens, matching current Huddle inline error styling.
- Starting/stopping: `Loader2` with `animate-spin`; the button is disabled until the transition settles.
- Use `transition-colors duration-150`. No continuous pulsing or waveform animation. Respect reduced-motion automatically by limiting motion to the existing loading spinner.
- Text truncates at one line. Main control may grow to its label but must not force participant content below one line; HuddleBar may collapse it to compact mode using its existing responsive behavior.

### States and copy

| Phase | Visible label | Icon | `aria-pressed` | Click | Tooltip / announcement |
|---|---|---|---:|---|---|
| `off` | Conversation | `AudioWaveform` | `false` | Start cutover | “Start Conversation mode” |
| `starting` | Starting… | `Loader2` | `false` | Disabled | “Starting native conversation audio” |
| `listening` | Listening | `AudioWaveform` | `true` | Stop mode | “Conversation mode is listening” |
| `thinking` | Thinking… | `Loader2` | `true` | Stop mode | “Your message was sent; waiting for an agent” |
| `speaking` | Speaking | `Volume2` | `true` | Stop mode | “Agent speaking; talk to interrupt” |
| `stopping` | Stopping… | `Loader2` | `true` | Disabled | “Stopping Conversation mode” |
| `error` | Retry | `TriangleAlert` | `false` | Retry start | Show the safe backend error and “Buzz restored standard Huddle audio.” |

Use a visually hidden `aria-live="polite"` span for transition announcements. Do not announce microphone level changes.

### Interaction rules

- Enter and Space use native button behavior.
- Starting is transactional: release WebView audio, attempt native start, and restore WebView audio if native start fails.
- Stopping is transactional: stop and join native audio first, then reacquire WebView audio. Until reacquisition succeeds, keep `isMuted=true` and show the existing microphone-unavailable state.
- The existing Mic button remains the mute control in both modes. While the bridge is active, its action updates the native bridge's muted flag instead of WebView transmission.
- Existing input-device selection is read-only while the bridge is active because VoiceProcessingIO follows the system/default route. The device drawer shows: “Conversation mode uses the system audio route. Stop it to choose a WebView input.”
- Existing Push-to-Talk/Voice Activity selection remains available. It controls the same Huddle gating and cancellation semantics after the producer changes.
- Existing Speaker controls remain authoritative for TTS enablement and voice selection.
- If models are still downloading, Conversation mode may start capture, but the status remains `starting` and the existing model progress UI remains the single source of progress. Do not add a second downloader.
- A Huddle leave always wins over an in-flight start. The start completion checks the Huddle session generation before installing the runtime.

### Accessibility and failure behavior

- Button has a stable accessible name that includes the current action, not only the current state.
- `aria-pressed` represents whether native audio owns the session.
- Error text is available in the tooltip and the existing Huddle error region; it is not color-only.
- Unsupported platforms omit the button entirely.
- Microphone permission denial retains Buzz's existing permission recovery UI.
- Native engine failure emits a stable error code for tests and a safe human-readable message for UI.
- Native-start failure says standard Huddle audio was restored and offers Retry; it does not expose internal paths, raw dependency errors, keys, transcripts, or channel identifiers.
- Bridge telemetry contains phases, durations, and error codes only—never audio, transcripts, keys, channel names or identifiers, relay URLs, device paths, or pubkeys.

### Product acceptance criteria

1. With Conversation mode off, existing Huddle behavior and tests are unchanged.
2. With Conversation mode on, WebView PCM is ignored and its microphone track is stopped.
3. Native 48 kHz mono capture reaches both existing Huddle consumers: STT and the audio relay when present.
4. Agent TTS is audible and the exact queued samples appear in the native render-reference stream.
5. Existing agent routing, voice selection, signing, and channel publication remain unchanged.
6. Talking over agent TTS cancels playback in under 50 ms on the validated Mac hardware.
7. Start failure restores standard Huddle audio without leaving a false active state.
8. Huddle leave and app shutdown stop the audio unit and worker threads.
9. The feature is absent on unsupported platforms, not merely disabled.
10. All unit, Desktop, Tauri, and full `just ci` gates pass.
11. The upstream PR remains draft and unmergeable until Buzz maintainers accept a maintained home for `buzztalk-audio` and the dependency manifest conforms to that decision.

---

## File Structure Map

### BuzzTalk source used as the reference implementation

- `crates/buzztalk-audio/src/vpio.rs` — VoiceProcessingIO engine.
- `crates/buzztalk-audio/src/lib.rs` — frame readers, playback handle, counters, and portable duplex contracts.
- `crates/buzztalk-core/src/lib.rs` — 48 kHz / 10 ms frame constants.
- `examples/buzz-desktop-integration/MicButton.tsx` — earlier proof of frontend state/event wiring; do not copy its key-file or second-relay architecture.
- `examples/buzz-desktop-integration/voice_commands.rs` — earlier proof of Tauri wiring; use only as a command/event pattern.

### Files created in `block/buzz`

- `desktop/src-tauri/src/huddle/buzztalk_bridge.rs` — state types, driver trait, macOS driver, worker lifecycle, command channel, and status snapshots.
- `desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs` — deterministic fake-driver tests.
- `desktop/src-tauri/src/huddle/audio_ingress.rs` — one shared 48 kHz mono fan-out used by WebView and native producers.
- `desktop/src-tauri/src/huddle/audio_ingress_tests.rs` — producer exclusivity and fan-out tests.
- `desktop/src/features/huddle/components/AudioBridgeButton.tsx` — presentational control.
- `desktop/src/features/huddle/lib/audioBridgeButtonState.ts` — pure state-to-view-model mapping.
- `desktop/src/features/huddle/lib/audioBridgeButtonState.test.mjs` — state/copy/accessibility tests.
- `desktop/src/features/huddle/lib/audioBridgeTransition.ts` — frontend transactional cutover state machine.
- `desktop/src/features/huddle/lib/audioBridgeTransition.test.mjs` — start, stop, rollback, and stale-generation tests.
- `desktop/docs/buzztalk-audio-dependency-decision.md` — Task 0 recommendation, ownership requirements, and maintainer disposition.
- `desktop/docs/buzztalk-audio-bridge.md` — operator behavior, feature gate, fallback, and hardware validation record.

### Files modified in `block/buzz`

- `desktop/src-tauri/Cargo.toml` and `Cargo.lock` — Apple Silicon macOS-only `buzztalk-audio` dependency from the Task 0 maintained source; a personal Git revision is draft-only.
- `desktop/src-tauri/src/huddle/mod.rs` — module registration, Tauri commands, teardown, and raw WebView ingress delegation.
- `desktop/src-tauri/src/huddle/state.rs` — serialized bridge snapshot plus non-serialized runtime handle.
- `desktop/src-tauri/src/huddle/audio_output.rs` — playback target abstraction.
- `desktop/src-tauri/src/huddle/tts.rs` and focused TTS tests — queue PCM through the selected playback target.
- `desktop/src-tauri/src/lib.rs` — import and register the two new Huddle commands in the existing `tauri::generate_handler!` list.
- `desktop/src/features/huddle/HuddleContext.tsx` — transactional producer cutover and command/event synchronization.
- `desktop/src/features/huddle/HuddleContext.types.ts` — expose bridge status and toggle.
- `desktop/src/features/huddle/components/HuddleBar.tsx` — render the new control in the existing cluster.
- `desktop/src/features/huddle/components/MicControls.tsx` — route mute/device behavior according to current producer.
- `desktop/src/testing/e2eBridge.ts` — expose deterministic mocked bridge status and command responses to Playwright.

### Additional test file created in `block/buzz`

- `desktop/tests/e2e/huddle-conversation-mode.spec.ts` — mocked start, stop, rollback, and visible-state coverage.

---

## Agent Execution Protocol

Each task is one review boundary. A Claude Sonnet or Codex Luna worker receives:

1. This document's header, Relationship to the Gateway Launcher, Global Constraints, and the single assigned task.
2. The latest branch SHA, Task 0 dependency disposition, and prior task's produced interface block.
3. Permission to edit only the task's listed files unless a compile error proves one adjacent registration file is required.
4. A requirement to leave a signed commit and a short evidence note containing commands and exit codes.

The coordinating agent reviews the diff and reruns the task's named gate before dispatching the next task. Execute Tasks 0–9 in order because each task consumes a decision, interface, or committed behavior from the preceding task. Physical hardware validation is never delegated and is limited to the Apple Silicon routes listed in Task 8; fake-driven CI does not replace it.

---

### Task 0: Record the dependency ownership recommendation

**Files:**
- Create: `desktop/docs/buzztalk-audio-dependency-decision.md`

**Decision gate:**
- Consumes: Buzz's dependency-ownership expectations, the minimal public surface required from `buzztalk-audio`, and the draft revision recorded above.
- Produces: one explicit recommendation among crates.io publication, vendoring into the Buzz monorepo, or another maintainer-approved home.
- The recommendation names the long-term owner and records versioning, update, security-response, license-provenance, and source-review expectations.
- A personal Git pin may be recorded only as a draft experiment mechanism, never as the merge recommendation.

- [ ] **Step 1: Compare the three ownership options**

Document operational ownership, release/update mechanics, security response, license provenance, and impact on Buzz's supply-chain review for each option. Do not choose based only on implementation speed.

- [ ] **Step 2: Write the recommendation and required next action**

Name one recommended option and the maintainer action needed to accept or reject it. If maintainers have not decided, mark the disposition `Open`, keep the PR draft, and allow Task 1 to use the pinned revision only for draft experimentation.

- [ ] **Step 3: Review the gate before source edits**

The coordinating agent confirms the document contains all required ownership fields and an explicit `Merge allowed: no` while disposition is open. No production source or Cargo manifest changes begin before this review.

- [ ] **Step 4: Commit**

```bash
git add desktop/docs/buzztalk-audio-dependency-decision.md
git commit -s -m "docs(desktop): recommend ownership for native audio dependency"
```

---

### Task 1: Establish the native bridge contract and capability state

**Files:**
- Modify: `desktop/src-tauri/Cargo.toml`
- Modify: `desktop/src-tauri/src/huddle/mod.rs`
- Create: `desktop/src-tauri/src/huddle/buzztalk_bridge.rs`
- Create: `desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs`

**Interfaces:**
- Consumes: `buzztalk_audio::VoiceProcessingEngine`, existing `HuddlePhase` and existing Huddle state event emission.
- Produces:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioBridgePhase {
    Off,
    Starting,
    Listening,
    Thinking,
    Speaking,
    Stopping,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AudioBridgeSnapshot {
    pub supported: bool,
    pub enabled: bool,
    pub phase: AudioBridgePhase,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

pub(crate) enum BridgeCommand {
    SetMuted(bool),
    Play48kMono(Vec<f32>),
    SetPhase(AudioBridgePhase),
    Stop,
}

pub(crate) trait NativeDuplexDriver: Send + 'static {
    fn try_capture_10ms(&mut self) -> Option<[f32; 480]>;
    fn queue_playback_48k(&mut self, samples: &[f32]) -> usize;
}
```

- `AudioBridgeSnapshot::unsupported()` returns `supported=false`, `enabled=false`, `phase=Off`, and no error.
- `AudioBridgeSnapshot::off_supported()` returns the same except `supported=true`.
- The production driver exists only under `cfg(all(target_os = "macos", target_arch = "aarch64"))`; fake drivers compile on every CI platform and unsupported targets return `AudioBridgeSnapshot::unsupported()`.

- [ ] **Step 1: Add the failing state-contract tests**

Test serialization strings, supported/off constructors, error reset, command-driven mute, and idempotent stop. Include this assertion shape:

```rust
#[test]
fn snapshot_serializes_with_frontend_phase_names() {
    let value = serde_json::to_value(AudioBridgeSnapshot::off_supported()).unwrap();
    assert_eq!(value["phase"], "off");
    assert_eq!(value["supported"], true);
    assert_eq!(value["enabled"], false);
}
```

Production code must avoid `unwrap()`; test code may use it.

- [ ] **Step 2: Run the focused test and observe RED**

Run:

```bash
. ./bin/activate-hermit
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
```

Expected: compile failure because the module and types do not exist.

- [ ] **Step 3: Add the Apple Silicon macOS-only dependency selected by Task 0**

Use the maintained source accepted in Task 0. If its disposition is still open, the following personal Git pin is allowed only on the draft experiment branch:

```toml
buzztalk-audio = { git = "https://github.com/mrobinson2/buzztalk", rev = "9fbbfc61260bb63c714a5c3694ec92cc8a602406" }
```

Do not add `buzztalk-pipeline`, `buzztalk-buzz`, STT, TTS, AEC, or relay dependencies.

- [ ] **Step 4: Implement the shared types and driver boundary**

Create the module, keep platform-independent types outside `cfg`, and add a macOS adapter that wraps `VoiceProcessingEngine::start(Default::default())`, `try_recv_capture()`, and `push_playback()` behind `NativeDuplexDriver`.

- [ ] **Step 5: Run focused and cross-platform compile gates**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
cargo check --manifest-path desktop/src-tauri/Cargo.toml
```

Expected: PASS on the current platform; unsupported targets never resolve Apple Silicon macOS-only symbols.

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/Cargo.toml desktop/src-tauri/Cargo.lock desktop/src-tauri/src/huddle/mod.rs desktop/src-tauri/src/huddle/buzztalk_bridge.rs desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs
git commit -s -m "feat(desktop): define native conversation audio bridge"
```

---

### Task 2: Create one capture-ingress seam for WebView and native audio

**Files:**
- Create: `desktop/src-tauri/src/huddle/audio_ingress.rs`
- Create: `desktop/src-tauri/src/huddle/audio_ingress_tests.rs`
- Modify: `desktop/src-tauri/src/huddle/mod.rs`
- Modify: `desktop/src-tauri/src/huddle/state.rs`

**Interfaces:**
- Consumes: existing `SttPipeline::push_audio`, `HuddleState.audio_relay_pcm_tx`, `HuddleState.manual_mic_unmuted`, and current 100 KB raw IPC limit.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioProducer {
    WebView,
    NativeBridge,
}

pub(crate) fn submit_huddle_pcm_48k(
    state: &AppState,
    producer: AudioProducer,
    samples: &[f32],
) -> Result<(), String>;
```

- The active producer lives in Huddle state and defaults to `WebView`.
- Samples from the inactive producer return `Ok(())` and reach zero consumers.
- Active, unmuted samples feed both STT and relay targets that exist.
- Active, muted samples feed neither consumer.
- `push_audio_pcm` parses and size-checks bytes, then calls this function with `AudioProducer::WebView`.

- [ ] **Step 1: Write RED tests for exclusivity and fan-out**

Use fake consumers or extracted pure fan-out inputs. Cover this truth table:

| Active producer | Submitted producer | Muted | STT calls | Relay calls |
|---|---|---:|---:|---:|
| WebView | WebView | false | 1 | 1 |
| WebView | NativeBridge | false | 0 | 0 |
| NativeBridge | NativeBridge | false | 1 | 1 |
| NativeBridge | WebView | false | 0 | 0 |
| either | matching | true | 0 | 0 |

- [ ] **Step 2: Run focused tests and observe RED**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::audio_ingress_tests
```

- [ ] **Step 3: Extract the current fan-out without changing behavior**

Move conversion/fan-out logic out of `push_audio_pcm`; retain its raw-body and maximum-size validation. Preserve exact current STT and relay behavior for the WebView default.

- [ ] **Step 4: Add producer selection to Huddle state**

Serialize the active producer only if the frontend needs it for diagnostics. Reset it to `WebView` in every Huddle reset/teardown path and copy no live handles in `Clone`.

- [ ] **Step 5: Run focused and existing Huddle tests**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::audio_ingress_tests
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle
```

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/src/huddle/audio_ingress.rs desktop/src-tauri/src/huddle/audio_ingress_tests.rs desktop/src-tauri/src/huddle/mod.rs desktop/src-tauri/src/huddle/state.rs
git commit -s -m "refactor(desktop): unify huddle audio ingress"
```

---

### Task 3: Run native capture through the existing Huddle consumers

**Files:**
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge.rs`
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs`
- Modify: `desktop/src-tauri/src/huddle/state.rs`
- Modify: `desktop/src-tauri/src/huddle/mod.rs`

**Interfaces:**
- Consumes: `submit_huddle_pcm_48k(..., AudioProducer::NativeBridge, ...)` from Task 2.
- Produces:

```rust
pub(crate) struct AudioBridgeRuntime {
    command_tx: std::sync::mpsc::Sender<BridgeCommand>,
    snapshot: std::sync::Arc<std::sync::RwLock<AudioBridgeSnapshot>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AudioBridgeRuntime {
    pub(crate) fn start(
        app: tauri::AppHandle,
        session_generation: u64,
    ) -> Result<Self, String>;

    pub(crate) fn command(&self, command: BridgeCommand) -> Result<(), String>;
    pub(crate) fn snapshot(&self) -> AudioBridgeSnapshot;
    pub(crate) fn stop(&mut self) -> Result<(), String>;
}
```

- Worker cadence is 10 ms or capture-driven, never a tight busy loop.
- Each capture submission checks the Huddle session generation and exits when stale.
- `Drop` requests stop and joins best-effort; explicit stop returns join failures.

- [ ] **Step 1: Write fake-driver RED tests**

Cover: capture reaches native ingress, muted frames are suppressed, stale generation stops the worker, two stop calls are harmless, and driver-start failure returns a stable `native_audio_start_failed` code.

- [ ] **Step 2: Run focused tests and observe RED**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
```

- [ ] **Step 3: Implement the worker with dependency injection**

Keep the worker loop generic over `NativeDuplexDriver`. The macOS constructor creates the real driver; tests pass a fake through a private `start_with_driver` constructor.

- [ ] **Step 4: Store the runtime without serializing or cloning it**

Add `audio_bridge_runtime: Option<AudioBridgeRuntime>` to Huddle state with `#[serde(skip)]`; cloned snapshots contain `None`. Reset and teardown call explicit stop before dropping state.

- [ ] **Step 5: Run tests and clippy**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
cargo clippy --manifest-path desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/src/huddle/buzztalk_bridge.rs desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs desktop/src-tauri/src/huddle/state.rs desktop/src-tauri/src/huddle/mod.rs
git commit -s -m "feat(desktop): feed huddle from native duplex capture"
```

---

### Task 4: Route existing TTS PCM through native playback

**Files:**
- Modify: `desktop/src-tauri/src/huddle/audio_output.rs`
- Modify: `desktop/src-tauri/src/huddle/tts.rs`
- Modify: `desktop/src-tauri/src/huddle/tts_tests.rs`
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge.rs`
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs`

**Interfaces:**
- Consumes: `BridgeCommand::Play48kMono(Vec<f32>)` from Task 1 and the current TTS worker's synthesized `Vec<f32>` before rodio playback.
- Produces:

```rust
pub(crate) trait HuddlePlaybackTarget: Send + Sync {
    fn play_mono(&self, samples: &[f32], sample_rate_hz: u32) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
}
```

- `RodioPlaybackTarget` preserves current behavior.
- `BridgePlaybackTarget` resamples to 48 kHz using the existing Buzz resampler where possible and sends `Play48kMono`.
- The active target is selected from Huddle state when each utterance begins; target changes cancel the current utterance before swapping.
- Existing per-speaker cancellation and `tts_cancel` remain authoritative.

- [ ] **Step 1: Write RED playback-target tests**

Assert that default mode calls rodio only; native mode sends the exact 48 kHz vector to the bridge and never opens rodio; `stop()` propagates to the active target; changing target cancels current speech.

- [ ] **Step 2: Run the focused TTS tests and observe RED**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::tts_tests
```

- [ ] **Step 3: Extract the playback target at the PCM boundary**

Keep synthesis, segmentation, voice caching, agent routing, and cancellation unchanged. Replace only the final device-write operation with `HuddlePlaybackTarget`.

- [ ] **Step 4: Add the native target**

Send samples through the bridge worker, which owns the mutable VoiceProcessingIO engine. Reject writes unless the bridge snapshot is enabled; return `native_audio_not_active` so callers can cancel rather than silently opening a second output.

- [ ] **Step 5: Run TTS, bridge, and Huddle tests**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::tts_tests
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle
```

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/src/huddle/audio_output.rs desktop/src-tauri/src/huddle/tts.rs desktop/src-tauri/src/huddle/tts_tests.rs desktop/src-tauri/src/huddle/buzztalk_bridge.rs desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs
git commit -s -m "feat(desktop): play huddle speech through native duplex audio"
```

---

### Task 5: Add transactional Tauri commands and lifecycle cleanup

**Files:**
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge.rs`
- Modify: `desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs`
- Modify: `desktop/src-tauri/src/huddle/state.rs`
- Modify: `desktop/src-tauri/src/huddle/mod.rs`
- Modify: `desktop/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: bridge runtime, active producer selection, playback target selection, Huddle generation, and existing `emit_huddle_state_changed` behavior.
- Produces commands:

```rust
#[tauri::command]
pub async fn set_buzztalk_audio_bridge_enabled(
    enabled: bool,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<AudioBridgeSnapshot, String>;

#[tauri::command]
pub fn get_buzztalk_audio_bridge_status(
    state: tauri::State<'_, AppState>,
) -> Result<AudioBridgeSnapshot, String>;
```

- Start preconditions: Apple Silicon macOS runtime capability, active Huddle phase, no existing runtime, current generation captured.
- Start commit order: snapshot `Starting` → construct runtime → set active producer NativeBridge → set playback target bridge → install runtime → snapshot `Listening` → emit state.
- Any failure before commit: stop partial runtime, restore WebView producer and rodio target, snapshot `Error`, emit state, and return a stable error code plus fixed recovery copy through the command contract. Raw CoreAudio, OS, device, dependency, and path details may be used transiently to select the code, but new bridge code neither logs nor serializes them.
- Stop order: snapshot `Stopping` → mute → cancel TTS → stop runtime → restore rodio target → restore WebView producer → snapshot `Off` → emit state.

- [ ] **Step 1: Write RED transaction tests**

Cover successful start/stop, repeated start/stop, start outside a Huddle, native constructor failure rollback, stale generation, leave-during-start, and shutdown teardown. Assert that constructor errors containing a fake device path, channel identifier, transcript fragment, or dependency URL are reduced to the stable code and safe recovery copy before serialization.

- [ ] **Step 2: Run focused tests and observe RED**

```bash
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle::buzztalk_bridge_tests
```

- [ ] **Step 3: Implement commands as explicit transactions**

Centralize state mutations in private `start_transaction` and `stop_transaction` functions so commands, Huddle leave, and app shutdown share the same ordering.

- [ ] **Step 4: Register commands and teardown hooks**

Follow the current `huddle` command-registration pattern. Do not introduce a separate Tauri plugin. Ensure Huddle teardown calls the bridge stop transaction before resetting audio state.

- [ ] **Step 5: Run native gates**

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
cargo test --manifest-path desktop/src-tauri/Cargo.toml huddle
cargo clippy --manifest-path desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/src/huddle/buzztalk_bridge.rs desktop/src-tauri/src/huddle/buzztalk_bridge_tests.rs desktop/src-tauri/src/huddle/state.rs desktop/src-tauri/src/huddle/mod.rs desktop/src-tauri/src/lib.rs
git commit -s -m "feat(desktop): manage conversation audio bridge lifecycle"
```

---

### Task 6: Implement the frontend cutover state machine

**Files:**
- Create: `desktop/src/features/huddle/lib/audioBridgeTransition.ts`
- Create: `desktop/src/features/huddle/lib/audioBridgeTransition.test.mjs`
- Modify: `desktop/src/features/huddle/HuddleContext.tsx`
- Modify: `desktop/src/features/huddle/HuddleContext.types.ts`
- Modify: `desktop/src/features/huddle/components/MicControls.tsx`

**Interfaces:**
- Consumes: Tauri commands from Task 5; existing WebView audio start/stop functions and Huddle generation/lifecycle.
- Produces additions to `HuddleContextValue`:

```ts
audioBridgeStatus: AudioBridgeStatus;
toggleAudioBridge: () => Promise<void>;
```

- Produces a testable transition dependency contract:

```ts
export interface AudioBridgeTransitionDeps {
  stopWebAudio(): Promise<void>;
  startWebAudio(): Promise<void>;
  startNative(): Promise<AudioBridgeStatus>;
  stopNative(): Promise<AudioBridgeStatus>;
  forceMuted(): Promise<void>;
  isCurrentGeneration(): boolean;
}

export async function enableAudioBridge(
  deps: AudioBridgeTransitionDeps,
): Promise<AudioBridgeStatus>;

export async function disableAudioBridge(
  deps: AudioBridgeTransitionDeps,
): Promise<AudioBridgeStatus>;
```

- Enable sequence: force muted → stop AudioWorklet → stop track → start native → confirm current generation. On failure, stop partial native and restart WebAudio before surfacing error.
- Disable sequence: force muted → stop native → start WebAudio. Unmute remains a separate user action.

- [ ] **Step 1: Write RED pure transition tests**

Assert exact call order for success and failure. Include:

```js
assert.deepEqual(calls, [
  "forceMuted",
  "stopWebAudio",
  "startNative",
]);
```

Failure must append `stopNative` and `startWebAudio`. A stale generation must end disabled and must not install returned active state.

- [ ] **Step 2: Run the focused Node tests and observe RED**

```bash
cd desktop && pnpm test -- src/features/huddle/lib/audioBridgeTransition.test.mjs
```

- [ ] **Step 3: Implement the pure transition module**

Use dependency injection; do not import React or Tauri in the pure module.

- [ ] **Step 4: Wire HuddleContext**

Map existing AudioWorklet/track lifecycle operations into the transition dependencies. Listen to existing Huddle state changes for backend status. On Huddle leave/unmount, disable bridge before disposing the context.

- [ ] **Step 5: Make MicControls producer-aware**

While bridge is enabled, `toggleMute` invokes the existing native manual-mute command and does not toggle AudioWorklet transmission. Disable WebView device selection and show the specified help copy.

- [ ] **Step 6: Run frontend tests and typecheck**

```bash
cd desktop
pnpm test -- src/features/huddle/lib/audioBridgeTransition.test.mjs
pnpm typecheck
pnpm check
```

- [ ] **Step 7: Commit**

```bash
git add desktop/src/features/huddle/lib/audioBridgeTransition.ts desktop/src/features/huddle/lib/audioBridgeTransition.test.mjs desktop/src/features/huddle/HuddleContext.tsx desktop/src/features/huddle/HuddleContext.types.ts desktop/src/features/huddle/components/MicControls.tsx
git commit -s -m "feat(desktop): switch huddle audio producers transactionally"
```

---

### Task 7: Build the Conversation button and state mapping

**Files:**
- Create: `desktop/src/features/huddle/components/AudioBridgeButton.tsx`
- Create: `desktop/src/features/huddle/lib/audioBridgeButtonState.ts`
- Create: `desktop/src/features/huddle/lib/audioBridgeButtonState.test.mjs`
- Modify: `desktop/src/features/huddle/components/HuddleBar.tsx`

**Interfaces:**
- Consumes: `AudioBridgeStatus` and `toggleAudioBridge` from Task 6.
- Produces pure view model:

```ts
export interface AudioBridgeButtonViewModel {
  label: string;
  accessibleName: string;
  tooltip: string;
  pressed: boolean;
  disabled: boolean;
  tone: "inactive" | "active" | "error";
  icon: "waveform" | "spinner" | "speaker" | "error";
}

export function audioBridgeButtonViewModel(
  status: AudioBridgeStatus,
): AudioBridgeButtonViewModel;
```

- [ ] **Step 1: Write RED table-driven view-model tests**

Create one case for every phase in the design table. Assert exact label, accessible name, pressed, disabled, tone, and icon values. Add an unsupported case that returns `null` through a separate `shouldShowAudioBridgeButton(status)` helper.

- [ ] **Step 2: Run focused test and observe RED**

```bash
cd desktop && pnpm test -- src/features/huddle/lib/audioBridgeButtonState.test.mjs
```

- [ ] **Step 3: Implement the pure mapping and presentational button**

Use exhaustive `switch` handling with a `never` assertion so adding a backend phase breaks typecheck until copy is defined. Apply the visual, responsive, tooltip, live-region, and accessibility contract from this document.

- [ ] **Step 4: Insert the control into HuddleBar**

Place it after `MicControls` and before `SpeakerControls`. Render only when supported. Pass `compact={mode !== "main"}` using HuddleBar's existing mode convention.

- [ ] **Step 5: Run focused and full Desktop gates**

```bash
cd desktop
pnpm test -- src/features/huddle/lib/audioBridgeButtonState.test.mjs
pnpm typecheck
pnpm check
```

- [ ] **Step 6: Capture design evidence**

Use Buzz's `desktop-screenshot` skill and capture these states in the same Huddle fixture: off, starting, listening, speaking, error, compact off, compact active, keyboard focus, and 200% zoom. Store screenshots according to upstream `AGENTS.md`; do not commit local scratch images unless its screenshot workflow requires it.

- [ ] **Step 7: Commit**

```bash
git add desktop/src/features/huddle/components/AudioBridgeButton.tsx desktop/src/features/huddle/lib/audioBridgeButtonState.ts desktop/src/features/huddle/lib/audioBridgeButtonState.test.mjs desktop/src/features/huddle/components/HuddleBar.tsx
git commit -s -m "feat(desktop): add Conversation mode control"
```

---

### Task 8: Verify rollback, cancellation, and end-to-end Huddle behavior

**Files:**
- Create: `desktop/docs/buzztalk-audio-bridge.md`
- Create: `desktop/tests/e2e/huddle-conversation-mode.spec.ts`
- Modify: `desktop/src/testing/e2eBridge.ts`

There are no planned production-code edits in this task. If a gate exposes a production defect, return the fix to the task that owns that file, rerun that task's focused gate, and create a separate signed fix commit before resuming Task 8.

**Interfaces:**
- Consumes: complete backend and frontend bridge.
- Produces: automated integration coverage, operator documentation, and hardware evidence.

- [ ] **Step 1: Add a mocked Tauri integration scenario**

The scenario starts a Huddle, confirms Conversation is off, toggles on, receives `starting` then `listening`, confirms the button and Mic state, toggles off, and confirms standard audio returns. Add a second scenario where native start rejects and assert the UI shows Retry plus the restored-audio message.

- [ ] **Step 2: Run the smallest E2E project containing the scenario**

```bash
cd desktop
pnpm test:e2e:smoke
```

Expected: PASS with deterministic mocked audio; no physical device assumptions.

- [ ] **Step 3: Run the full automated gates**

```bash
. ./bin/activate-hermit
cargo test --manifest-path desktop/src-tauri/Cargo.toml
cd desktop && pnpm test && pnpm typecheck && pnpm check
cd .. && just ci
```

Expected: every command exits 0. Record exact failing commands and fixes in the execution log; rerun the complete final list after the last fix.

- [ ] **Step 4: Perform macOS hardware validation**

This step is required and non-delegated. Perform it only on the validated Apple Silicon Mac and only on these routes:

1. Built-in microphone + headphones.
2. One Bluetooth headset used for both input and output.
3. Open loudspeakers.
4. AirPods/headset disconnect and reconnect while active.
5. Native-start failure induced by denying microphone permission, followed by standard-path recovery.
6. Huddle leave while agent speech is active.

For each route, record: native start result, transcription correctness, audible TTS, self-interruption count, three intentional barge-in latencies, and whether teardown releases the device.

Pass criteria: zero false self-interruptions in the short validation script; each deliberate interruption silences TTS within 50 ms; route reconnect recovers or degrades to standard Huddle audio with an explicit message. Do not extrapolate these observations to Windows, Linux, Intel macOS, iOS, or any unlisted route.

- [ ] **Step 5: Write operator documentation**

Document the Apple Silicon macOS-only scope, user-visible states, fallback behavior, telemetry boundaries, feature gate, known limitations, dependency decision and revision, and hardware results. Explicitly state that Conversation mode does not change keys, messages, models, or relay topology. The bridge creates no separate log file or rotation mechanism; bridge diagnostics added to existing Buzz logging contain stable phase/error metadata only, while Buzz's existing retention policy remains authoritative.

- [ ] **Step 6: Commit**

```bash
git add desktop/docs/buzztalk-audio-bridge.md desktop/tests/e2e/huddle-conversation-mode.spec.ts desktop/src/testing/e2eBridge.ts
git commit -s -m "test(desktop): verify Conversation mode integration"
```

Confirm `git diff --cached --name-only` contains exactly those three paths before committing.

---

### Task 9: Prepare the upstream PR without broadening scope

**Files:**
- No planned repository file changes. Route any review fix back to its owning task and commit it separately with sign-off.
- Create PR body in a temporary file outside the repository.

**Interfaces:**
- Consumes: all prior signed commits and verification evidence.
- Produces: one reviewable upstream PR.

- [ ] **Step 1: Rebase onto current upstream main with sign-off**

```bash
git fetch upstream main
git rebase --signoff upstream/main
```

Resolve conflicts by preserving current upstream Huddle behavior when Conversation mode is off. Rerun affected task gates after every conflict resolution.

- [ ] **Step 2: Run final verification from a clean index**

```bash
. ./bin/activate-hermit
just ci
git diff --check upstream/main...HEAD
git log --format='%h %s%n%b' upstream/main..HEAD
```

Pass criteria: `just ci` and diff check exit 0; every commit contains `Signed-off-by`.

- [ ] **Step 3: Self-review against the acceptance criteria**

For each of the eleven product acceptance criteria, cite one automated test, hardware observation, explicit unsupported-platform branch, or accepted ownership record. Fix any criterion without evidence before opening the PR.

- [ ] **Step 4: Open a draft PR**

The PR body must contain:

- Problem: split WebView capture and Rust playback prevent a shared render reference.
- Scope: Apple Silicon macOS Huddle audio ownership only; no identity, relay, STT/TTS, model, or message changes.
- UX: Conversation button states and fallback.
- Safety: producer exclusivity, transactional rollback, feature/capability gate.
- Evidence: automated commands, screenshots, and physical-route table.
- Dependency: link the Task 0 decision, identify the accepted maintainer and source, and show that the manifest conforms. If disposition remains open, identify the exact draft-only BuzzTalk revision and state `Merge allowed: no`.
- Explicit non-goals: iOS, Android, Windows/Linux native bridge, speech-stack deduplication.

The PR remains draft while dependency ownership is open. A personal Git revision is sufficient only for draft experimentation and is never merge-ready.

- [ ] **Step 5: Stop at the review boundary**

Do not merge, publish, or alter upstream release configuration without explicit repository-owner authorization. Do not mark the PR ready for review or request merge while Task 0 says `Merge allowed: no`, while ownership is unnamed, or while the manifest still points to an unaccepted personal Git home.

---

## Done Definition

This plan is complete only when:

- The Conversation control matches every specified state in main and compact Huddle surfaces.
- Native and WebView capture never submit concurrently.
- Native capture and playback are proven through automated fakes and real macOS hardware.
- Standard Huddle audio is the tested rollback path, not an assumption.
- Existing Buzz identity, relay, messages, models, agent routing, STT, TTS, and cancellation remain the sources of truth.
- All commits carry DCO sign-off and `just ci` passes after the final rebase.
- The draft upstream PR includes evidence and declares the external dependency decision openly.
- Buzz maintainers have accepted a maintained home and named owner for `buzztalk-audio`; the manifest conforms to that decision. Until then, the PR remains draft and this plan is not merge-complete.

## Skeptical Review Notes

- **Dependency gate:** the personal Git pin is permitted only for a working draft experiment and is not an acceptable merge state. Task 0 requires a recommendation among crates.io publication, vendoring into the Buzz monorepo, or another maintainer-approved home; the PR remains draft until maintainers accept the owner and source.
- **Largest technical risk:** current TTS playback code may couple synthesis and rodio more tightly than the indexed boundary suggests. Task 4 protects the rest of the plan by extracting one small playback trait and proving current rodio behavior before adding the native target.
- **Largest product risk:** two audio owners during transition. Tasks 2, 5, and 6 make producer selection and rollback explicit and independently tested.
- **Largest disclosure risk:** raw native errors can contain device or dependency details. Task 5 requires fixed recovery copy and tests that raw paths, identifiers, transcript fragments, and URLs do not cross the Tauri boundary.
- **What this plan intentionally avoids:** embedding the full BuzzTalk conversation pipeline, handling keys outside Buzz, launching a sidecar daemon, replacing Buzz's mature Huddle speech stack, or pretending unvalidated platforms are supported.
