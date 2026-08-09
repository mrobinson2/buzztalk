# Bump sherpa-onnx 1.12 → 1.13 — WITHDRAWN

**Status: not proposed. The premise was wrong.**

## What this claimed

That bumping `sherpa-onnx` from "1.12" to "1.13" in `desktop/src-tauri/Cargo.toml` and
`crates/buzz-voice/Cargo.toml` would unlock `OnlineRecognizer` (streaming recognition),
`VoiceActivityDetector` with Silero, and `OfflineTtsPocketModelConfig` for Buzz.

## Why it was wrong

`"1.12"` in Cargo is a caret requirement — it means `^1.12`, i.e. any `1.x >= 1.12`.
Buzz's lockfiles already resolve to **1.13.4**, in both the root workspace and
`desktop/src-tauri`:

```
name = "sherpa-onnx"
version = "1.13.4"
```

Buzz has been compiling against 1.13.4 all along. Every API this document claimed the bump
would unlock is already available to Buzz today. Changing the declared floor does not move
the resolved dependency graph by a single byte — verified: `git diff -- Cargo.lock
desktop/src-tauri/Cargo.lock` is empty on the branch that makes the change.

## Conclusion

A pull request that edits a version string, changes nothing about what gets compiled, and
justifies itself by "unlocking" APIs that were never locked is noise. It would cost
maintainer review time and deliver nothing. Withdrawn.

## What remains true

The underlying observation still stands and is worth raising separately if anyone wants it:
Buzz's STT uses `OfflineRecognizer` and only decodes after ~300 ms of trailing silence, so
nothing appears on screen while the user is speaking. `OnlineRecognizer` is already
available in the version Buzz compiles. That is a feature proposal, not a dependency bump,
and it should be argued on its own terms with a working implementation attached.

## Lesson

Check what the lockfile actually resolves before proposing a version bump. A caret
requirement means the declared version is a floor, not the version in use.
