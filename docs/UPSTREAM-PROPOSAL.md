# Proposal: BuzzTalk as Buzz's voice conversation engine

*Draft for block/buzz maintainers — not yet filed. 2026-08-09.*

## The one-sentence pitch

Buzz mutes the microphone while an agent speaks; BuzzTalk removes the
reason, and we'd like to upstream it as the crate that gives Buzz a mic
button with real barge-in.

## The constraint this removes

Buzz's voice stack is excellent — Parakeet TDT-CTC via sherpa-onnx,
Kyutai Pocket TTS, VAD, push-to-talk, per-agent voices — but capture
lives in the webview (`getUserMedia`) while synthesis plays from the Rust
process (`rodio`). Two audio clients, no shared reference signal, so the
echo canceller has nothing to subtract, and the only safe policy is to
discard every mic frame while an agent talks. Buzz does exactly that, and
it is the right call *given the constraint*.

BuzzTalk removes the constraint: one engine owns capture and playback on
one clock, so the exact samples sent to the speaker exist as an
echo-cancellation reference. That single change turns Buzz's existing
speech engines into an interruptible conversation.

## Evidence (all measured, logs in-repo)

Everything below was measured 2026-08-09 on real hardware against a real
`buzz-relay` and a live agent (`buzz-acp` + `claude-agent-acp`); raw logs
and the full session record are in
`docs/live-session-2026-08-09/SESSION-REPORT.md`.

| Claim | Number |
|---|---|
| Barge-in against a live agent, headphone path | 19.5–43 ms (8 events) |
| Barge-in through live loudspeakers, AEC active | **7.2 / 33.1 / 39.6 ms**, zero false self-triggers |
| Barge-in via VoiceProcessingIO on a Bluetooth headset | 20.0 ms |
| End of speech → final transcript | 37–235 ms typical |
| Per-frame compute (release) | 481 µs of a 10 ms budget |
| Full-duplex on ONE Bluetooth headset (mic + in-ear replies) | working, word-perfect transcription |

For contrast: cloud assistants take roughly 700 ms to stop talking after
you start. The loudspeaker interruptions matter most — the barge-in gate
only opens on measured echo suppression (ERLE), so those firings are
themselves evidence the canceller worked against a real acoustic echo.

## What Buzz users would get

A mic toggle in the channel header. While it's on: speech becomes
ordinary signed `kind:9` messages from the logged-in identity (live
partials rendering in the composer), agent replies are synthesized with
the existing per-agent voices, and **talking over an agent silences it in
tens of milliseconds** — conversation, not an IVR menu.

Attribution is the identity model Buzz already has: messages are signed
with the app's key, so spoken and typed words are indistinguishable on
the wire. Nothing new to trust.

Multi-agent channels stay listenable by design: the engine takes a
*speakable list* — which agents get read aloud — so one narrator voice
can render a working crew ("reviewer found two issues; builder is fixing
the first") instead of every agent talking at once. Dispatcher, not
conference call.

## Architecture offered

Ten Apache-2.0 crates (matching Buzz's license), 261 tests. The ones that
matter for this proposal:

- `buzztalk-audio` — duplex engine with a bit-exact render-reference tap.
  Two backends: cpal (all platforms) and, new, **`VoiceProcessingIO`
  (macOS)** — one audio unit for both directions, which is what makes
  Bluetooth headsets work (two independent CoreAudio clients starve a BT
  mic to digital silence; we measured it) and is the same API iOS will
  use. Includes a device watchdog: stream errors, default-device or
  sample-rate changes, and capture stalls trigger an in-place engine
  rebuild in ~1 s — a headset power-cycle mid-conversation self-heals.
- `buzztalk-vad` — separate endpoint and barge-in detectors (turn-taking
  is permissive, interruption is strict and ERLE-gated).
- `buzztalk-aec` — pluggable cancellers; `sonora` default, chosen by
  bake-off with numbers (`docs/PHASE-0.md`), including the finding that
  the reference WebRTC APM under-reports its own ERLE by ~35 dB, which
  would silently disable any ERLE-gated barge-in built on it.
- `buzztalk-session` / `buzztalk-pipeline` — the turn state machine and
  orchestrator: preroll, endpointing (tunable, e.g. 700 ms pause
  tolerance), barge-in retraction, empty/punctuation-only transcript
  guards, best-partial fallback when a final decode returns blank.
- `buzztalk-stt` / `buzztalk-tts` — wrappers over the same engines Buzz
  ships. The TTS crate is a port of Buzz's own; upstreaming deletes the
  duplication rather than adding a parallel stack.

## Proposed phasing

1. **Adopt `buzztalk-audio` (+ vpio) as a shared crate.** Smallest PR,
   immediately useful: it's the piece that fixes Bluetooth duplex and
   device-change resilience for anything that plays or records audio in
   the Rust process. No behavior change for existing features.
2. **Mic button behind a feature flag.** Desktop app wires a
   `ConversationPipeline` to the active channel, signing with the app
   identity; live partials in the composer; speakable list defaults to
   the channel's agents. Buzz's existing mute-while-speaking path stays
   the fallback when the flag is off.
3. **Deduplicate the speech stacks.** BuzzTalk's STT/TTS wrappers merge
   with Buzz's; one set of engines, one model-download story. iOS follows
   (VoiceProcessingIO is the same API there); Android needs an
   Oboe/AAudio backend and platform AEC — honest gap, not started.

## Honest limitations

- The 36.6 dB ERLE figure is synthetic; the *functional* loudspeaker
  claim is proven, but the controlled acoustic bench number hasn't been
  re-measured on hardware yet.
- Agent replies arrive as complete messages today; streaming replies
  would cut the ~10–15 s LLM round-trip feel and is designed but not
  built.
- One engine, one key, one speaker: no diarization. Two humans on one
  mic are one author.
- Endpointing is a fixed (configurable) silence threshold; semantic
  endpointing ("complete thought → reply sooner") is the obvious next
  quality jump and is on our roadmap either way.
- Web client voice is out of scope here — different architecture
  (WASM or a native helper), and desktop + mobile are where Buzz lives.

## What we're asking

Interest check first. If the direction reads right, we'd open the phase-1
PR (shared audio crate) and a tracking issue for the mic button. Repo:
https://github.com/mrobinson2/buzztalk — everything above is reproducible
from the logs and docs there.
