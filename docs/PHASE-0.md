# Phase 0 — Architecture validation

**Objective.** Answer the one question that can sink the build, before writing product code:
*does acoustic echo cancellation work well enough, and with which crate?*

Everything else in the plan is bounded, well-understood work. This is not.

## Environment (measured 2026-08-08)

| | |
|---|---|
| Machine | macOS 24.6, Apple Silicon (arm64) |
| Rust | 1.97.1, installed via rustup with `--no-modify-path` (use `~/.cargo/bin/cargo`) |
| C/C++ | Apple clang 17.0.0 |
| **cmake** | **not installed** |
| **meson / ninja** | **not installed** |
| Homebrew | 6.0.15, available |
| Node / pnpm | 26.0.0 / 11.1.3 |
| Docker | 29.4.0, daemon running |
| Free disk | ~34 GiB — tight for a Tauri monorepo `target/` |

The missing cmake and meson matter: `webrtc-audio-processing` builds a C++ library and is
the candidate most likely to need them. That was flagged as a risk in the architecture
document and it is now a measured fact rather than a guess. Installing a build toolchain is
a decision to make deliberately, not a side effect of a `cargo check`.

## Gates

Phase 0 exits when all four hold:

1. One echo-cancellation backend chosen **with numbers written down**, not preference.
2. Synthetic ERLE > 20 dB for the chosen backend.
3. The control (passthrough) scores ~0 dB — proof the harness is not flattering anything.
4. Duplex capture + playback runs 60 s with no xruns and no drift.

## Harness

`crates/buzztalk-labs` — not shipped, exists to produce evidence.

```
cargo run -p buzztalk-labs --bin erle-bench
```

Method: synthesise a speech-like far-end (band-limited noise, 4 Hz syllable envelope,
phrase-level pauses); derive a near-end by delaying it 40 ms, attenuating 12 dB, adding two
reflections, applying a soft-clip non-linearity and a −60 dBFS noise floor; feed far-end to
`process_render` and near-end to `process_capture`; score energy reduction over the final
50% of the run so convergence transients do not inflate the number.

The non-linearity is deliberate. It is exactly why naively subtracting the known playback
signal fails on real hardware, and a backend that only handles the linear case will look
excellent here and disappoint in a room.

### Control result

| backend | ERLE (dB) |
|---|---|
| passthrough (control) | 0.0 |

Harness validated: doing nothing measures as doing nothing.

## Backend bake-off — RESULT

| candidate | version | builds | needs cmake/meson | ERLE (dB) | notes |
|---|---|---|---|---|---|
| `webrtc-audio-processing` | 2.1.0 | **no** | **yes** | — | Dynamic path needs `pkg-config` (absent). `bundled` feature needs `meson` (absent): *"Failed to execute meson."* Not installed — reporting, not silently fixing. |
| `sonora` | 0.2.0 | yes | no (pure Rust) | **36.6** | `AudioProcessing` is natively `Send`; API maps ~1:1 onto `EchoCanceller`. MSRV 1.91. |
| `aec3` | 0.3.1 | yes | no (pure Rust) | **35.2** | `LinearPipeline` holds an `Rc`-based graph runtime and is **not `Send`**, which collides with `EchoCanceller: Send`. Needs a worker thread + channels to use at all. |
| passthrough | — | — | — | **0.0** | Control. |

Measured twice with different excitation, and the gap between the two real backends is not
stable: on spectrally-tilted broadband noise they separated by ~4.4 dB (20.0 vs 15.6), on
the harness above — which adds three reflections and a soft-clip non-linearity — they land
within 1.4 dB of each other (36.6 vs 35.2). **The dB difference is signal-dependent and is
not a sound basis for choosing.** Both clear the gate by a wide margin.

Two findings worth keeping:

- A swept-tone chirp made *both* real backends look broken (≈0 dB). That is AEC3-family
  behaviour correctly declining to adapt on ill-conditioned narrowband excitation, not a
  defect. Any future benchmark must use broadband excitation or it will draw the wrong
  conclusion.
- Noise suppression and AGC2 are disabled in both wrappers. AGC2 re-normalises output
  loudness, which would mask the energy reduction being measured and make the numbers
  meaningless.

### Re-test after installing meson + ninja

`webrtc-audio-processing` 2.1.0 **does** build via its `bundled` feature once `meson` and
`ninja` are present (it never needed cmake). ~28 s to rebuild on an 8-core Apple Silicon
box with a warm cargo registry; realistically 1–3 minutes cold on a CI runner. That cost is
paid by every contributor and all three CI platforms, on every clean build, forever.

Measured on the harness above — reflections plus soft-clip non-linearity, i.e. the harder
of the two signals:

| backend | ERLE measured | ERLE **self-reported** |
|---|---|---|
| `sonora` | **36.6 dB** | 28.4 dB |
| `aec3` | 35.2 dB | 26.6 dB |
| `webrtc` | 35.1 dB | **0.2 dB** |
| passthrough | 0.0 dB | — |

All three land within 1.5 dB. The reference WebRTC APM — the implementation both pure-Rust
crates are reproducing — does not win, on either the simple signal or the hard one.

**The decisive finding is in the right-hand column, not the left.** `webrtc` reports 0.2 dB
of echo return loss enhancement while actually delivering 35.1 dB. Its internal metric does
not track reality.

That is not cosmetic. `BargeInDetector` gates acoustic interruption on `AecStats::erle_db`
and suppresses detection below a 12 dB floor, because a canceller that is not keeping up
means anything "heard" is probably the agent's own voice. A backend that under-reports ERLE
by 35 dB would permanently fail that gate — cancelling echo perfectly well while silently
disabling barge-in, which is the entire product. It would present as "voice works but
interruption doesn't", on some machines and not others, and would be extremely hard to trace
back to a stats accessor.

`sonora` (28.4 vs 36.6 measured) and `aec3` (26.6 vs 35.2) both under-report modestly and
directionally correctly, which is what the gate needs.

### Decision: default to `sonora`

Not because it measured higher — that margin is within signal noise. Because it is `Send`
and needs no thread-confinement workaround, so it is the lower-risk integration. `aec3`
stays available behind a feature as a second opinion.

`webrtc` remains available behind `backend-webrtc` for anyone who wants the reference
implementation, but it is not the default and its `stats()` reporting must be fixed before
it could be. The prediction recorded here earlier — that the real APM would beat both ports
once it could build — was tested and did not hold on either signal.

Synthetic ERLE is still the weakest evidence in this document. The real test is a laptop in
a room, and it has not been run.

## Duplex engine — RESULT

`buzztalk-audio`: cpal 0.18 input + output, lock-free SPSC rings (ringbuf 0.5), exact
480-sample framing, drop-and-count on overrun, macOS route detection via raw CoreAudio
(`AudioObjectGetPropertyData` on `kAudioDevicePropertyTransportType`, disambiguating
built-in via `kAudioDevicePropertyDataSource` because CoreAudio has no distinct transport
type for the headphone jack).

The requirement that makes or breaks the product: the exact samples written to the output
device must also be published as a gap-free reference stream, including the silence written
when nothing is queued. A reference with holes in it is a canceller that diverges. This is
enforced by two tests — `render_reference_matches_playback_bit_exactly_at_native_rate` and
`silent_output_still_publishes_to_render_reference`.

17 tests pass (14 offline, 3 hardware). Offline coverage: ring wraparound, framing of
arbitrary-length input, silence round-trip, overrun counters, resampler behaviour.

### Gate 4 — 60 s duplex soak, run on real devices

```
ran 60.0s
capture frames      : 6001
render-ref frames   : 5999
skew (cap - ref)    : +2 frames (20.0 ms)
capture dropped     : 0 samples
render-ref dropped  : 0 samples
playback dropped    : 0 samples
playback underrun   : 0 samples
GATE 4 PASS — duplex stable, reference locked to capture.
```

Skew held at +1 to +2 frames for the whole run rather than growing, so there is no clock
drift between capture and reference at this configuration. That is the number to watch: a
reference that slips relative to capture destroys echo cancellation slowly, and the symptom
appears much later as "barge-in randomly stopped working".

The harness paces playback against the wall clock. An earlier unpaced run pushed ~8×
real time and dropped 12.9 M samples — the counter caught it, which is itself evidence the
backpressure path works, but the underrun figure only means something when pacing is honest.

## Environment caveat — this machine cannot validate acoustics

The development machine is a **Mac mini accessed remotely via Jump Desktop**. Its default
audio device is a virtual driver (Phase Five Systems LLC, 8-in/8-out), not physical
hardware:

```
input : ["Jump Desktop Microphone", "Jump Desktop Audio"]
output: ["Mac mini Speakers", "Jump Desktop Microphone", "Jump Desktop Audio"]
detect_output_route() -> unknown
```

`unknown` is the correct answer for a virtual device, and it degrades safely: `Unknown` is
treated as `Speakers`, so the ERLE gate stays armed rather than assuming a headphone free
pass. But three consequences follow, and none of them are code problems:

1. **There is no acoustic loop here.** Every echo-cancellation number in this document is
   synthetic. The 36.6 dB figure says the algorithm works; it does not say the product works
   in a room.
2. **The headphone fast path is untested on real hardware.** It is the demo's safety net, so
   it needs a physical Mac with headphones plugged in before it can be relied on.
3. **The launch demo cannot be recorded on this machine.** It needs a physical Mac, a real
   microphone, and real speakers or headphones.

Phase 1 should be validated on physical hardware before Phase 5 (barge-in) is trusted.

## Decisions taken here

- **48 kHz f32 mono internally, 10 ms frames.** Matches the microphone open rate, Opus's
  native rate, AEC3's expected quantum, and the tick rate Buzz's existing playout loop and
  TTS barge-in monitor already use. One cadence everywhere.
- **`buzztalk-core` holds no I/O and no engine dependencies.** The `EchoCanceller` and
  `SpeechDetector` traits live there; every backend is a sibling crate behind a feature.
- **Unknown output route is treated as speakers.** Assuming an echo path that is not there
  costs a little barge-in latency; assuming one that is there costs false interruptions,
  which is the failure that makes people switch voice off.

## Phase 1 integration — duplex + AEC on real device timing

`cargo run --release -p buzztalk-labs --bin live-aec --features aec-backends`

The offline bench proves the algorithm cancels. This proves the plumbing: that render
reference and capture frames arrive paired and in order, and that the canceller keeps up at
the 10 ms cadence. Those are different failures — a canceller fed correct audio in the wrong
order produces exactly the same symptom as one that does not work.

| | release | debug |
|---|---|---|
| worst single-frame AEC time | **481–616 µs** | **7182 µs** |
| share of the 10 ms real-time budget | ~5–6% | **~72%** |
| capture / render-ref drops | 0 | 0 |
| playback underruns | 0 | 0 |
| frame pairing backlog | stable at 0/1 | stable at 0/1 |

**Voice must run in release builds.** A debug build consumes nearly three quarters of the
real-time frame budget before any speech recognition or synthesis is added — it would appear
to work in isolation and fall apart the moment STT and TTS share the machine. Release leaves
roughly 20× headroom, which is what the remaining pipeline gets to spend.

ERLE is reported as `n/a` here rather than a number: this machine's virtual input device
captures digital silence, so there is no echo to remove. That is the expected result on this
hardware and it is why the acoustic claims still need a physical Mac.
