# Mic button in Buzz desktop — integration guide

The desktop equivalent of the upstream proposal's phase 2: a mic toggle in
Buzz's channel view, backed by BuzzTalk's engine running *in Buzz's own
Tauri process* — no separate `buzztalkd`. Two drop-in files plus one small
addition to BuzzTalk.

Buzz's desktop app is Tauri (Rust backend, web frontend), so this is a
Rust-to-Rust integration — cleaner than the FFI the mobile port needs.

## Files in this folder

- **`voice_commands.rs`** → copy to `desktop/src-tauri/src/commands/`. Three
  Tauri commands (`buzztalk_start` / `buzztalk_stop` / `buzztalk_is_active`)
  wrapping `buzztalk-pipeline` + `buzztalk-buzz`, streaming pipeline events
  to the frontend as `buzztalk://event`.
- **`MicButton.tsx`** → copy to `desktop/src/features/chat/ui/`. The toggle:
  live partials into the composer draft, pipeline phase drives the label
  (listening / thinking / speaking), barge-in automatic.

## Wiring steps

1. **Add deps** to `desktop/src-tauri/Cargo.toml`:
   ```toml
   buzztalk-pipeline = { git = "https://github.com/mrobinson2/buzztalk" }
   buzztalk-buzz      = { git = "https://github.com/mrobinson2/buzztalk" }
   buzztalk-session   = { git = "https://github.com/mrobinson2/buzztalk" }
   ```
   Ship the Parakeet + Kyutai models with the app (or reuse Buzz's — it
   already bundles both) and point `BUZZTALK_MODELS_DIR` at them.

2. **Register** in the Tauri builder (`lib.rs` / `main.rs`):
   ```rust
   .manage(commands::voice_commands::VoiceState::default())
   .invoke_handler(tauri::generate_handler![
       /* …existing… */,
       commands::voice_commands::buzztalk_start,
       commands::voice_commands::buzztalk_stop,
       commands::voice_commands::buzztalk_is_active,
   ])
   ```

3. **Render** `<MicButton …/>` in the channel header/composer, passing the
   active channel's uuid, the relay url, the agent pubkey to speak, and a
   path to a file holding the logged-in user's nsec. Buzz already holds the
   key in its keyring — write it to an app-private file on start and delete
   on stop, or (better) add a `KeySource::Keyring` variant so the raw key
   never touches disk.

## The one BuzzTalk addition needed

`ConversationPipeline` today exposes `recv_event_timeout(&self)`. The event
pump wants to *own* the receiver on its own thread without locking the
state mutex. Add to `buzztalk-pipeline`:

```rust
pub fn take_event_rx(&mut self) -> Option<Receiver<PipelineEvent>>
```

**Added — this method now exists in `buzztalk-pipeline`.** It hands the
event `Receiver` to an external pump (returns `None` if already taken);
after which `recv_event_timeout` returns `None`. The example uses it
directly.

## Windows / Linux — testing voice off macOS

macOS uses VoiceProcessingIO (`use_voice_processing: true`); everywhere
else `MicButton.tsx` passes `false` and the engine uses the portable cpal
`DuplexEngine`. That path **compiles but is unproven** — this is the chance
to prove it.

**A built-in laptop mic is the ideal first Windows test**: it's a wired
device, so the Bluetooth-mic-starvation problem that forced VPIO on macOS
doesn't apply, and the two-stream cpal engine should just work.

Quick check before the full app, straight from the BuzzTalk repo on the
Windows box:

```
cargo build --release -p buzztalk-buzz --bin buzztalkd
buzztalkd --download-models
buzztalkd --relay wss://<community>.communities.buzz.xyz \
  --channel <uuid> --agent-pubkey <pk> --key-file key.txt \
  --endpoint-silence-ms 700         # no --vpio on Windows
```

Speak. What to watch for, all first-time-on-Windows unknowns:
- Does cpal open the built-in mic and deliver 48 kHz frames? (It should.)
- Route detection returns `Unknown` on Windows, so the barge-in gate
  assumes an acoustic echo path and leans on the AEC — with laptop speakers
  + mic that's the honest case; with headphones pass `--headphones`.
- The AEC (`sonora`) has only ever run against macOS-captured audio;
  validate barge-in actually fires and doesn't self-trigger on speaker
  bleed.

If the built-in-mic path works, the mic button works on Windows with
`use_voice_processing: false`. If barge-in misbehaves, that's the
platform-AEC work item, not a blocker for push-to-talk-style use.

## What this does not include

Building and shipping the modified Buzz app (their build, signing,
release). This is the code and the wiring; landing it is a Buzz-repo PR —
the upstream proposal (`docs/UPSTREAM-PROPOSAL.md`) is the pitch for it.
