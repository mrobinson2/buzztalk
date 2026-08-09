# iOS voice port plan

*Draft, 2026-08-09. How BuzzTalk voice reaches the iPhone Buzz app — and
why today's work already did most of the hard part.*

## The claim this plan rests on

The architectural risk in voice — full-duplex capture and playback on one
clock, with a bit-exact echo reference, working over Bluetooth, with
sub-40 ms barge-in — is **already retired**, on the exact API iOS uses.
The macOS engine shipped this session (`buzztalk-audio`'s
`VoiceProcessingEngine`) is built on Apple's `kAudioUnitSubType_VoiceProcessingIO`
audio unit, which exists **identically on iOS**. So this is a port and an
integration, not a research problem. Everything below is bounded work.

## What moves, what's reused, what's new

| Layer | Status for iOS |
|---|---|
| `buzztalk-core`, `buzztalk-session`, `buzztalk-vad`, `buzztalk-pipeline` | **Reused as-is.** Pure Rust, no platform audio. The whole turn machine, endpointing, barge-in gate, dead-capture watchdog, preroll — all cross-compile to `aarch64-apple-ios` unchanged. |
| `buzztalk-audio` VoiceProcessingIO path | **~90% reused.** Same audio unit, same `coreaudio-rs`. iOS differences are `AVAudioSession` setup and lifecycle, not the unit itself. |
| `buzztalk-stt` (Parakeet / sherpa-onnx) | **Reused, needs iOS build of onnxruntime.** sherpa-onnx ships iOS artifacts; Buzz already runs Parakeet on desktop. Model is ~126 MB. |
| `buzztalk-tts` (Kyutai Pocket) | **Reused, needs iOS build.** ~158 MB. |
| `buzztalk-buzz` (relay transport, signing) | **Reused.** tungstenite + rustls cross-compile; wss already works. |
| FFI bridge (Rust ↔ Flutter/Dart) | **New.** The Buzz mobile app is Flutter. Expose start/stop/partial-transcript/state over `flutter_rust_bridge` or a thin `dart:ffi` C ABI. |
| Mic button + composer UI | **New.** A record toggle in the channel view; live partials into the composer; the same UX the upstream desktop proposal describes. |

## The five real pieces of work

### 1. Cross-compile the crates to iOS
- Targets `aarch64-apple-ios` (device) and `aarch64-apple-ios-sim`.
- The blocker is the two native model runtimes: onnxruntime (Parakeet, via
  sherpa-onnx) and whatever Kyutai TTS links. Both have iOS builds; the
  work is wiring their `.xcframework`s into the Cargo build and the app.
- `cpal` is not needed on iOS if we go VoiceProcessingIO-only (recommended
  — see §3). That removes cpal's iOS caveats entirely.

### 2. `AVAudioSession` — the one genuinely iOS-specific chunk
macOS opens the audio unit and goes. iOS gates all audio behind an
`AVAudioSession` the app must configure and defend:
- Category `.playAndRecord`, mode `.voiceChat` (this mode *is* what routes
  through the system AEC and gives Bluetooth HFP the right treatment).
- Options `.allowBluetooth`, `.defaultToSpeaker`, `.duckOthers`.
- Handle **interruptions** (phone call, Siri, another app grabbing audio)
  and **route changes** (AirPods connect/disconnect, unplug) via
  `AVAudioSession.interruptionNotification` / `routeChangeNotification`.
  This is the iOS analogue of the device watchdog we built — and it maps
  onto the same `rebuild_engine` path: a route change or interruption-end
  tears down and reopens the unit.
- **Background audio**: decide whether voice keeps running when the app
  backgrounds (needs the `audio` background mode entitlement) or pauses.
  Recommend pause-on-background for v1 — simpler, and matches user
  expectation for a push-to-talk-ish feature.

### 3. Go VoiceProcessingIO-only on iOS
On iOS, do **not** offer the two-stream cpal fallback. VoiceProcessingIO
is the only sane path: it's the one that survives Bluetooth (the whole
reason it exists), and it hands us Apple's AEC for free, which matters more
on a phone's tiny speaker-to-mic spacing than anywhere. This also means the
barge-in gate leans on Apple's echo suppression rather than sonora on iOS —
validate ERLE behaviour there, it may let us relax our own gate.

### 4. FFI bridge + Flutter integration
- `flutter_rust_bridge` is the path of least resistance for a Flutter app;
  it generates the Dart bindings from annotated Rust.
- Surface a tiny API: `start(relay, channel, agentPubkeys, keyRef)`,
  `stop()`, and a stream of events (`Partial`, `FinalTranscript`,
  `StateChanged`, `AudioDeviceRebuilt`, barge-in metrics) — the same
  `PipelineEvent`s the daemon already emits.
- **Key handling**: the app already holds the user's signing key in the iOS
  keychain. Pass a *reference*, not the raw key, across the FFI boundary if
  possible; the Rust side asks the app to sign, or receives the key in
  secure memory and zeroes it. Do not let the key transit Dart strings.

### 5. UI: the mic button
- Toggle in the channel header. On: request mic permission
  (`NSMicrophoneUsageDescription`), start the session, stream live partials
  into the composer so the user sees words forming, publish on endpoint.
- Off / interrupt: the barge-in already works at the engine level; the UI
  just reflects state (listening / speaking / thinking).
- Reuse the desktop UX from the upstream proposal so both platforms feel
  the same.

## Sequencing

1. **Prove the engine on-device first, headless.** Cross-compile, run a
   tiny test harness in a bare iOS app that opens VoiceProcessingIO, does
   the `--simulate`-style loopback, and confirms 48 kHz capture + the
   render-reference tap on real hardware. This de-risks §1–§3 before any
   Flutter work.
2. **STT/TTS on-device.** Get Parakeet and Kyutai running in that harness;
   measure per-frame compute on an actual iPhone (the 481 µs desktop
   number will be different — validate the 10 ms budget holds).
3. **FFI + minimal Flutter screen.** One channel, hardcoded, mic button,
   partials in a text field. First end-to-end voice message from a phone.
4. **AVAudioSession hardening.** Interruptions, route changes, Bluetooth,
   background policy. This is where most of the *surprises* live.
5. **Fold into the Buzz mobile app proper** — same as the upstream desktop
   proposal's phase 2, mobile edition.

## Honest unknowns

- **On-device model compute.** Parakeet + Kyutai both run on desktop CPU
  comfortably; a phone is weaker and thermally limited. Likely fine on
  recent iPhones (Neural Engine / accelerated onnxruntime), but the 10 ms
  real-time budget must be re-measured, not assumed. This is the single
  biggest open question.
- **App Store audio-session etiquette.** `.voiceChat` mode and background
  audio draw review scrutiny; nothing blocking, but get the entitlements
  and usage strings right early.
- **Battery.** Continuous mic + on-device inference is not free. Push-to-
  talk / tap-to-start (not always-listening) is the battery-honest default.
- **Android is a separate effort.** No VoiceProcessingIO; needs an
  Oboe/AAudio backend plus Android's `AcousticEchoCanceler`. Same
  `buzztalk-audio` trait boundary, different implementation. Not in this
  plan.

## Bottom line

Nothing here is speculative engineering. The turn machine, barge-in,
endpointing, and self-healing are done and portable. The audio unit is the
same one iOS uses. The two native model runtimes have iOS builds. The work
is: cross-compile, wrap the audio unit in an `AVAudioSession`, bridge to
Flutter, add a button — in that order, proving each on real hardware before
the next. The hard question that could have sunk it — full-duplex
interruptible voice over Bluetooth — was answered on macOS this session
with the same API iOS will use.

---

## Build status (started 2026-08-09)

First cutting session. What actually compiles for `aarch64-apple-ios`:

| Crate | iOS cross-compile | Notes |
|---|---|---|
| `buzztalk-core`, `buzztalk-session`, `buzztalk-vad` | **compiles** | Pure Rust, first try, zero changes. The turn machine, endpointing, and barge-in gate port as-is. |
| `buzztalk-audio` incl. `VoiceProcessingEngine` | **compiles** | VPIO cfg extended from `macos` to `any(macos, ios)`; new `configure_ios_audio_session()` (category `.playAndRecord`, mode `.voiceChat`, DuckOthers+DefaultToSpeaker) via `objc2-avf-audio`, gated `cfg(ios)`. The CoreAudio HAL (device enum / route detection) stays macOS-only; `route.rs` already returns `Unknown` on iOS. **The hard part — the full-duplex Bluetooth engine — compiles for the phone.** |
| `buzztalk-ffi` (new) | builds on host | C ABI over the pipeline for Flutter `dart:ffi`: `buzztalk_start / poll_event / string_free / stop`, key passed as a file path never a raw string. Its iOS *link* waits on the STT blocker below. |
| `buzztalk-pipeline` / `buzztalk-buzz` / `buzztalk-ffi` | **blocked** | Two independent blockers, both predicted, neither in our code. |

### The two blockers (both external, both expected)

1. **No full Xcode.** This machine has Command Line Tools only; `xcrun`
   can't locate the `iphoneos` SDK. Any dependency with a C/asm build
   script (e.g. `ring`) fails to cross-compile. Fix: install Xcode. Pure
   Rust and the objc2 binding crates don't need it, which is why the audio
   engine compiles and `ring` doesn't.
2. **STT runtime has no iOS build wired.** `buzztalk-stt` → `sherpa-onnx`
   → `sherpa-onnx-sys`, whose build downloads/links onnxruntime and pulls
   `ureq`/`rustls`/`ring`. It has no iOS target support out of the box.
   Fix: link a prebuilt iOS `onnxruntime.xcframework` and teach
   `sherpa-onnx-sys` (or a shim) to use it instead of its host downloader.
   This is the single biggest remaining task and the one the on-device
   compute question (10 ms budget) rides on.

### Net

The architectural risk retired earlier — full-duplex interruptible voice
over Bluetooth on VoiceProcessingIO — now also **compiles for iOS**. What
remains before a phone can run it is toolchain (install Xcode) and the STT
runtime's iOS build, plus TTS the same way, then the FFI links and the
Flutter mic-button integration begins. No new architectural unknowns
surfaced this session.
