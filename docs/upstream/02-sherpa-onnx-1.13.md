# Bump sherpa-onnx 1.12 → 1.13

**Size:** two lines. **Files:** `desktop/src-tauri/Cargo.toml:137`, `crates/buzz-voice/Cargo.toml:20`

## Why

1.13 adds API surface Buzz can use directly, independent of any downstream project:

- `OnlineRecognizer` / `OnlineStream` — true streaming recognition. Buzz's STT is currently
  `OfflineRecognizer` decoding after ~300 ms of trailing silence, so nothing appears while
  the user speaks. A streaming Zipformer would give live partial transcripts.
- `VoiceActivityDetector` with `SileroVadModelConfig` — a more robust VAD in noise than the
  current `earshot` detector, running on the ONNX runtime already loaded.
- `OfflineTtsPocketModelConfig` — first-class Pocket TTS config, worth evaluating against
  Buzz's hand-rolled April engine.

## Verified downstream

BuzzTalk runs sherpa-onnx 1.13.4 in production code with Parakeet TDT-CTC 110M, the same
model Buzz ships, with no API breakage in the offline path Buzz uses today. Measured
re-decode of a 3-second buffer: 137–148 ms.

## Caveat found while integrating

If a crate links both sherpa-onnx and `ort`, the two must share one ONNX Runtime.
`buzz-voice` already does this correctly with `ort-sys`'s `disable-linking` plus `api-24`.
Worth preserving deliberately on any bump: a second runtime in the same process segfaults
at construction, and the failure looks nothing like a version-mismatch bug.
