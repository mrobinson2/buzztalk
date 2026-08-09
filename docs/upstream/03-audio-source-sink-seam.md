# Make huddle audio capture and playback pluggable

**Size:** ~200 lines. **Files:** `desktop/src-tauri/src/huddle/{mod,stt,tts,audio_output}.rs`

## Problem, on Buzz's own terms

Huddle audio has exactly one capture path — the webview's `getUserMedia` and AudioWorklet,
arriving via `push_audio_pcm` — and one playback path, a `rodio::Player` created inside the
TTS worker. Neither can be substituted, which has two costs for Buzz today:

1. **Testing.** `desktop/tests/e2e/huddle-transcription.spec.ts` has to fake `getUserMedia`
   in the browser to exercise transcription. There is no way to drive the STT pipeline from
   a WAV in a Rust test, so the resample → VAD → Parakeet path has no fast, deterministic
   coverage.
2. **Echo cancellation.** Capture lives in the webview and playback in the Rust process, so
   the webview's echo canceller has no reference for Buzz's own TTS. The STT worker
   documents this and discards every microphone frame while TTS plays
   (`huddle/stt.rs:435-448`). That is the correct call given the constraint, but the
   constraint is structural, and it is what makes acoustic barge-in impossible.

## Proposed change

Introduce two traits with the current behaviour as the default implementations:

```rust
pub trait AudioSource: Send {
    /// Deliver captured frames. 48 kHz f32 mono, as push_audio_pcm already expects.
    fn try_recv(&mut self) -> Option<Vec<f32>>;
}

pub trait AudioSink: Send {
    /// Accept synthesized audio for playback.
    fn push(&mut self, samples: &[f32]);
}
```

`WebviewSource` wraps the existing `push_audio_pcm` channel; `RodioSink` wraps the existing
player. Add an `AppState` slot so an alternative can be installed at startup. Default
behaviour is byte-for-byte unchanged.

## What it unlocks for Buzz

- Rust-level tests that feed a WAV through the real STT pipeline, no webview.
- A native duplex path where capture and playback share one clock, which is the
  precondition for a true echo-cancellation reference and therefore for barge-in.

## Evidence this is worth it

BuzzTalk implements that native duplex path and measures, on a virtual device: 60 s with
zero dropped frames and no clock drift, 481 µs of a 10 ms budget per frame in release,
36.6 dB of synthetic echo return loss enhancement, and barge-in to silence in 7–15 ms.
Those numbers are not achievable while capture and playback live in different processes.

**Caveat, stated plainly:** those measurements are synthetic and on a virtual audio device.
They demonstrate the architecture is sound, not that it is validated in a room.
