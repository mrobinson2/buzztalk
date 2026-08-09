# Acoustic validation — run this on a real Mac

Every echo-cancellation number in this repository is **synthetic**. They were measured
against a simulated echo path on a machine whose audio device is a virtual driver, so no
microphone has ever heard a real loudspeaker here.

Those numbers show the algorithm is sound. They do not show the product works in a room,
and the gap between the two is the entire feature: barge-in depends on the canceller
subtracting real speaker output leaking into a real microphone, through real
non-linearity, real reflections, and real clock drift between two physical devices.

This is the last unproven claim in the project. It takes about two minutes to close.

## Run it

On a Mac with a real microphone and real speakers, in a reasonably quiet room:

```
git clone https://github.com/mrobinson2/buzztalk && cd buzztalk
cargo run --release -p buzztalk-labs --bin hw-validate --features aec-backends
```

It prompts you to plug and unplug headphones, plays speech-band noise, and prints a
PASS/FAIL verdict for four checks:

| check | why it matters |
|---|---|
| a real input device exists | guards against measuring a virtual driver and believing the result |
| route detection tells headphones from speakers | the headphone fast path is the demo's safety net; if this is wrong, barge-in is gated on ERLE even when no echo path exists |
| the mic actually hears the speaker | if it hears nothing there is no echo to cancel, and a high ERLE figure is meaningless rather than good |
| **real-world ERLE ≥ 12 dB** | the threshold `BargeInDetector` gates on; below it, acoustic barge-in is suppressed by design |
| self-reported ERLE tracks measured | the gate reads the *reported* value — a backend that under-reports disables barge-in while cancelling perfectly. This is exactly how the `webrtc` backend failed here (0.2 dB reported vs 35.1 dB actual) |

## Then confirm by ear

Automation cannot judge the last part:

```
cargo run --release -p buzztalk-pipeline --bin buzztalk-demo -- --seconds 60
```

Speakers on, no headphones. Let the agent start talking, then interrupt it out loud. If it
stops, barge-in works in a room.

## After it passes

1. Replace "synthetic" with the measured numbers in `README.md` and `docs/PHASE-0.md`.
2. Drop the `-alpha` suffix only once the by-ear test passes too.
3. Keep the "use headphones" guidance if ERLE lands between 12 and 20 dB — that is working,
   but not comfortably.

## If it fails

Do not weaken the warnings in the release notes. A failure here is useful information: it
most likely means the room, the speaker volume, or the delay estimate needs work, not that
the architecture is wrong. Try `--no-aec` on the demo to confirm the rest of the pipeline
is healthy independent of cancellation.
