# Controlled acoustic measurement — run this on a real Mac

BuzzTalk's **functional** acoustic path is proven: during the 2026-08-09 live session, a
real microphone heard agent speech from real loudspeakers and three deliberate
interruptions stopped playback in 7.2, 33.1, and 39.6 ms with no observed false
self-interruptions. See the
[session report](live-session-2026-08-09/SESSION-REPORT.md).

The remaining gap is quantitative. The repository's **36.6 dB ERLE** figure comes from a
simulated echo path, not a controlled physical-room bench run. This harness measures that
number on real hardware and checks that the canceller's self-reported ERLE is trustworthy
enough for the barge-in gate. It supplements the live functional evidence; it does not
erase or repeat it.

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

## Optional functional confirmation

The live session already confirmed this behavior, but a new device or room can be checked
by ear:

```
cargo run --release -p buzztalk-pipeline --bin buzztalk-demo -- --seconds 60
```

Speakers on, no headphones. Let the agent start talking, then interrupt it out loud. If it
stops, barge-in works in a room.

## After a controlled run

1. Add the measured hardware number alongside the synthetic and live-functional results in
   `README.md` and `docs/PHASE-0.md`; do not rewrite the historical measurements.
2. Keep headset-first guidance if ERLE lands between 12 and 20 dB — that is working,
   but not comfortably.
3. Keep the `-alpha` suffix until packaging and physical Windows/Linux audio coverage match
   the project's intended support statement.

## If it fails

Do not replace the pending controlled-ERLE caveat with a passing claim. A failure here is
useful information: it most likely means the room, speaker volume, or delay estimate needs
work, not that the live functional record disappeared. Try `--no-aec` on the demo to
confirm the rest of the pipeline is healthy independent of cancellation.

## The harness has been validated against a machine that should fail

In the pre-live development environment, whose only input was a virtual audio driver,
`hw-validate` failed all five checks and refused to produce a usable number:

```
inputs : ["<virtual microphone>", "<virtual audio device>"]
  captured echo   : -120.0 dBFS
  MEASURED ERLE   : -24.3 dB   (backend self-reports Some(0.176))

  [FAIL] real input device present            every input looks like a virtual driver
  [FAIL] route detection distinguishes ...    speakers => unknown, headphones => unknown
  [FAIL] microphone actually hears the speaker  -120.0 dBFS captured while playing
  [FAIL] real-world ERLE >= 12 dB             -24.3 dB
  [FAIL] self-reported ERLE tracks measured   reported 0.2, measured -24.3

SOME CHECKS FAILED — no controlled hardware ERLE claim is available
```

The −24.3 dB figure is meaningless: with no acoustic path, the microphone captures digital
silence and the "ERLE" is just noise arithmetic. That is precisely why the harness checks
whether the microphone hears anything *before* reporting cancellation, and fails loudly
instead of printing a number that looks like a measurement.

A validator that cannot fail proves nothing. This one fails where it should.
