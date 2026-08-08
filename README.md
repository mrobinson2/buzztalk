# BuzzTalk

**Natural voice conversations for [Buzz](https://github.com/block/buzz).**

Talk to your agents. Hear them respond. Interrupt them naturally.
Speech runs locally on your machine.

> **Status: pre-alpha.** The audio foundation is built and measured; there is nothing to
> install yet, and it does not talk to Buzz yet.

## Where this actually is

| | |
|---|---|
| Echo cancellation | `sonora` chosen over `aec3` and `webrtc-audio-processing`. **36.6 dB** synthetic ERLE against a **0.0 dB** passthrough control. |
| Duplex engine | 60 s soak: zero dropped frames, zero underruns, **no clock drift** between capture and the render reference. |
| Real-time budget | **481 µs** per frame in release against a 10 ms budget — ~20× headroom. (A debug build takes 7182 µs. Run voice in release.) |
| Render reference | Bit-exact with device output, silence included, enforced by test. |
| Tests | 22 passing, plus 3 hardware tests behind `--ignored`. |

**Not yet proven:** every echo-cancellation number above is synthetic. The development
machine is a remotely-accessed Mac mini whose default audio device is a virtual driver, so
it has no acoustic loop and cannot validate the thing that matters most — a real microphone
hearing a real speaker in a real room. See [`docs/PHASE-0.md`](docs/PHASE-0.md).

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

Planned: `buzztalk-vad`, `buzztalk-stt`, `buzztalk-tts`, `buzztalk-session`,
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

| | Status |
|---|---|
| macOS (Apple Silicon) | Tier 1 — primary development target |
| Windows | Tier 1 — intended, not yet validated |
| Linux | Tier 2 — audio-stack dependent; headphones recommended |

## Licence

Apache-2.0, matching Buzz. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE) for third-party
attribution, including the CC-BY-4.0 obligations that travel with the speech models.
