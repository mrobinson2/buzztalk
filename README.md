# BuzzTalk

**Natural voice conversations for [Buzz](https://github.com/block/buzz).**

Talk to your agents. Hear them respond. Interrupt them naturally.
Speech runs locally on your machine.

> **Status: alpha** — [v0.1.0-alpha.1](https://github.com/mrobinson2/buzztalk/releases/tag/v0.1.0-alpha.1).
> The full conversation loop works and talks to a real Buzz relay. The acoustic path is
> **not** yet validated on physical hardware — use headphones. See
> [`docs/HARDWARE-VALIDATION.md`](docs/HARDWARE-VALIDATION.md).

## Where this actually is

| | |
|---|---|
| Echo cancellation | `sonora` chosen over `aec3` and `webrtc-audio-processing`. **36.6 dB** synthetic ERLE against a **0.0 dB** passthrough control. |
| Duplex engine | 60 s soak: zero dropped frames, zero underruns, **no clock drift** between capture and the render reference. |
| Real-time budget | **481 µs** per frame in release against a 10 ms budget — ~20× headroom. (A debug build takes 7182 µs. Run voice in release.) |
| Render reference | Bit-exact with device output, silence included, enforced by test. |
| Barge-in gating | Fires on speech in 4 frames (**40 ms**). Rejects broadband coughs, keyboard clicks and door slams. |
| Barge-in → playback silent | **7.1 – 15.5 ms** end to end |
| End-of-speech → final transcript | 72 – 203 ms |
| TTS warm synthesis | ~4× real time |
| Tests | **36 passing**, plus 3 hardware tests behind `--ignored`. |

**Proven end to end against a real Buzz relay.** `buzztalkd` authenticated over NIP-42,
published a signed `kind:9` event with the correct `h` and `p` tags, and the message was
read back using Buzz's own CLI.

**Not yet proven:** every echo-cancellation number above is synthetic. The development
machine is a remotely-accessed Mac mini whose default audio device is a virtual driver, so
it has no acoustic loop and cannot validate the thing that matters most — a real microphone
hearing a real speaker in a real room. **Barge-in over loudspeakers is unverified.** See
[`docs/PHASE-0.md`](docs/PHASE-0.md) for the full measurement record.

---

## Why this exists

Buzz already has excellent local speech. It ships NVIDIA Parakeet TDT-CTC 110M for
recognition via sherpa-onnx, Kyutai Pocket TTS with nine bundled voices, voice-activity
detection, push-to-talk, per-agent voice assignment, hash-pinned model downloads, and an
Opus/NetEQ conferencing transport. It even has a cancellation path that silences playback
about 15 ms after a flag is set, mid-sentence.

What it does not have is the ability to **interrupt an agent by speaking**.

The reason is architectural, not a missing feature. Buzz captures the microphone in the
webview (`getUserMedia`) and plays synthesized speech from the Rust process (`rodio`).
Two audio clients, no shared reference signal — so the echo canceller has nothing to
subtract, and the only safe policy is to discard every microphone frame while the agent
is talking. Buzz does exactly that, and it is the correct call given the constraint.

BuzzTalk removes the constraint. It takes ownership of the audio device so capture and
playback live in one process with one clock, which means the exact samples sent to the
speaker are available as an echo-cancellation reference. That single change is what turns
Buzz's existing engines into a conversation.

**BuzzTalk is a conversation layer, not another speech stack.** It does not reimplement
recognition or synthesis. It owns the microphone, the speaker, the turn, and the
interruption.

## Architecture

```
cpal duplex engine (one process, one clock)
  ├─ capture ──► AEC ◄── render reference (the exact output samples)
  │               ├─► barge-in detector (strict, ERLE- and route-gated)
  │               ├─► pre-roll ring (500 ms, so the first word survives)
  │               └─► endpoint detector ──► speech recognition
  └─ output  ◄── speech synthesis
```

| Crate | Role |
|---|---|
| `buzztalk-core` | Types and traits. Zero I/O, zero engine dependencies. |
| `buzztalk-audio` | cpal duplex engine, lock-free rings, render-reference tap, output-route detection. |
| `buzztalk-aec` | `EchoCanceller` implementations, feature-gated, plus a null fallback. |
| `buzztalk-vad` | Endpoint and barge-in detectors, ERLE- and route-gated. |
| `buzztalk-labs` | Measurement harnesses. Not shipped; exists to produce evidence. |

Planned: `buzztalk-stt`, `buzztalk-tts`, `buzztalk-session`,
`buzztalk-buzz`, `buzztalk-telephony`, `buzztalkd`.

## Design rules

- `buzztalk-core` never imports an engine or an I/O crate. If it does, the abstraction has broken.
- Audio callbacks allocate nothing, lock nothing, log nothing.
- Backends are Cargo features. Users compile out what they do not want.
- Every failure degrades to something usable. Losing echo cancellation must not lose voice;
  losing voice must not lose Buzz.
- Interruption latency is a tested number, not an impression. A regression there is a build failure.

## Privacy

Audio processing is local. No network in the audio path. No raw audio is written to disk —
ring buffers only, in memory. Transcripts become ordinary Buzz messages with ordinary Buzz
retention; BuzzTalk keeps no separate transcript store. Logs record pipeline stage and
status, never spoken text.

## Platforms

| | Builds + tests in CI | Audio validated on real hardware |
|---|---|---|
| macOS (Apple Silicon) | yes | **no** — dev machine has only a virtual audio device |
| Windows | yes | **no** |
| Linux | yes | **no** |

CI is green on all three, which means the code compiles everywhere and the offline suite
passes everywhere. It does **not** mean voice works everywhere: no CI runner has an audio
device, so every hardware path sits behind `#[ignore]`. Route detection is implemented for
macOS only and returns `Unknown` elsewhere, which degrades safely to assuming an echo path.

## Licence

Apache-2.0, matching Buzz. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE) for third-party
attribution, including the CC-BY-4.0 obligations that travel with the speech models.
