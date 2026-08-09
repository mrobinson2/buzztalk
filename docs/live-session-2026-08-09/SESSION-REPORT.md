# Live hardware session — 2026-08-09

The first end-to-end use of BuzzTalk as an actual product: a human on real
hardware (Sony WH-CH720N Bluetooth headset, Mac mini) holding a spoken,
interruptible, multi-turn conversation with a live Claude agent through a
real Buzz relay. Every claim below has a raw log in this directory.

## What was proven live

- **Full conversation loop on real hardware.** Speech captured from a real
  microphone, transcribed locally, published as kind:9, answered by a live
  agent (`buzz-acp` + `claude-agent-acp`), the reply synthesized and played
  in the speaker's ears. Dozens of complete turns.
- **Barge-in against a live agent: 8 events, 19.5–43.0 ms** from voice
  detected to playback silent (`--headphones` route, so gating only — the
  loudspeaker echo path remains unvalidated). The 7–15 ms synthetic number
  becomes ~20–43 ms over Bluetooth output latency. Cloud assistants sit
  near 700 ms.
- **End-of-speech → final transcript: 37–235 ms typical** across the
  session (outliers to ~680 ms on long utterances).
- **Multi-sentence turns**: with 700 ms endpoint patience, two spoken
  sentences with a natural gap arrived as one message.
- **Agent verbatim recall by voice**: asked "tell me exactly what you heard
  in the last sixty seconds," the agent reproduced all three prior
  utterances word-for-word, in order.
- **The agent debugged its own input channel.** Told the speaker its
  message "started mid-word with 'iv I should say', so the wake phrase
  itself is being swallowed" — a correct live diagnosis of front-clipping.
  Turn-taking QA performed inside the product being QA'd.

Session totals: 22 published utterances, 17 empty transcripts correctly
refused before publish (the empty-content guard earned its keep), 8
barge-ins, zero crashes of the session loop after the idle-restart fix.

## Bugs found (in the order they bit)

1. **Host-app mic permission (fixed by config).** macOS TCC applies to the
   spawning app: the daemon inherited a *denied* mic from its terminal host
   while Buzz.app's own grant was irrelevant. Symptom: frames arrive,
   all-silence, no error anywhere. `tccutil reset Microphone <host bundle>`
   re-prompts.
2. **Daemon deafness after 90 s of silence (fixed in code).** The session
   machine's `IDLE_TIMEOUT` ends the session; nothing restarted it, so
   `buzztalkd` stayed alive but deaf forever. Fix: `buzztalkd` now
   restarts the session on `Idle` — Listening is the steady state.
3. **Bluetooth sample-rate renegotiation breaks the long-lived capture
   stream (OPEN — top reliability item).** When TTS playback starts/stops
   on the same BT headset, the headset renegotiates its audio format. New
   readers (Sound Settings meter, ffmpeg probes) open at the new rate and
   look healthy; buzztalkd's stream, opened at the old rate, is fed
   time-warped audio. Energy-based VAD still fires; the recognizer decodes
   garbage ("The trare", a lone "c") or nothing. Evidence: every daemon
   restart (fresh stream) instantly restored transcription on the same
   untouched headset; a parallel ffmpeg capture measured healthy −13 dB
   peaks during a dead phase. **Fix design: listen for CoreAudio device
   format/configuration-change notifications and rebuild capture/playback
   streams in place.** This also covers device switching (headset connect
   mid-session killed the output stream and the pipeline with it).

   **This fix is a launch gate, not a nice-to-have.** Product decision
   (2026-08-09): BuzzTalk is wireless-first — the realistic user is on a
   Bluetooth headset or, eventually, an iPhone with AirPods. A design that
   is only reliable on wired microphones is a wrong design. Wired mics are
   a *diagnostic bench tool* (they isolate variables, and the loudspeaker
   AEC measurement wants one for controlled conditions), never a user
   requirement.
4. **Front-clipping after quiet gaps (OPEN — partially mitigated).** First
   words after silence are lost ("I said can you hear me" → "can you hear
   me"; "BuzzTalk activate" → "talk activate"). The BT mic path delivers
   digital silence while waking, so no preroll size can replay what was
   never captured. Preroll raised 500 ms → 1500 ms to cover what *is*
   captured. Because wireless is the target platform, this needs product-
   and engine-level answers, not "use a wired mic": hold the Bluetooth
   capture link warm (an always-active input stream should pin HFP/SCO up
   — investigate why the deep sleep still occurred with our stream open),
   and/or an explicit wake word, which absorbs the clipped opening by
   design — notably, the speaker reached for one instinctively.

## Tuning changes shipped this session

- `--endpoint-silence-ms` flag on `buzztalkd`, plumbed through
  `PipelineConfig` into `EndpointConfig.hangover_frames`. Detector default
  stays ~300 ms; live testing ran 700 ms, which held two-sentence turns
  together. External review (OpenAI/Deepgram guidance) suggests 500 ms as
  a shipping default with 300 ms too aggressive for thinking-aloud speech.
- `PREROLL_DURATION` 500 ms → 1500 ms (see bug 4).
- Both changes: all affected crate tests pass (one stale magic-number test
  updated to track the constant).

## Roadmap items surfaced by live use

- **Adaptive/semantic endpointing** — complete thought → ~350 ms, trailing
  "…the weather in" → ~900 ms. The turn-taking feel, more than raw
  latency, is what separates a voice command interface from a
  conversation; this is the differentiator item.
- **Barge-in and end-of-turn stay independent timers** (already true:
  BargeInDetector ~40 ms confirm vs EndpointDetector) — keep it that way.
- **Mic keep-alive / stream rebuild** (bug 3) before any Bluetooth user
  sees this.
- Streaming agent replies (unchanged from before; reply latency is the
  Claude round-trip, ~10–15 s conversational).

## Afternoon session (same day): self-healing engine + LOUDSPEAKER BARGE-IN PROVEN

The morning's top two open items both closed the same day.

**Self-healing audio engine shipped.** `buzztalk-audio` now surfaces
stream-error callbacks as an engine failure flag and exposes a
default-device signature (`default_devices_signature`); the orchestrator
polls both at 1 Hz and rebuilds the engine in place — fresh streams at the
device's current rate, fresh canceller, fresh detector/recognizer state —
on stream error, default-device/sample-rate change, or capture stall (one
attempt per stall). Directions pinned to an explicit device are masked out
of the signature so macOS's constant default-flipping doesn't churn
rebuilds. Observed live: headset power-cycled mid-session → watchdog
rebuilt onto the fallback device on disconnect and back onto the headset
on reconnect, zero manual intervention. New flag: `--output-device NAME`.
All 257 workspace tests pass.

**New bug isolated (the real Bluetooth killer): duplex-on-BT starves the
mic.** With both capture and playback streams open on the BT headset, the
mic delivers pure digital silence (measured peak 0.000 via the new
`capture-dump` labs tool) while input-only capture on the same untouched
device measures real speech. This — not just rate renegotiation — is why
transcription kept dying. Buzz's webview path never hits it because
Apple's voice-processing audio unit manages BT as one session. **Fix
direction, now the top engine item: a VoiceProcessingIO-based engine mode
(single unified input+output session, with Apple's AEC as a bonus) — the
same work iPhone/AirPods support needs.** Interim: split routing (BT mic
in, wired speakers out).

**Loudspeaker barge-in: PROVEN.** The split routing is precisely the
launch-gate configuration — agent speech from real loudspeakers into the
room, echo cancellation live (no `--headphones`), the speaker's mic
hearing everything. The user deliberately interrupted the agent
mid-sentence through the open-air echo path three times:

    barge-in -> playback silent:  7.2 ms   (matches the 7-15 ms synthetic range)
    barge-in -> playback silent: 33.1 ms
    barge-in -> playback silent: 39.6 ms

No false-positive self-interruptions were observed while the agent spoke
from the speakers. Note the gate's design did its job: barge-in candidates
are only trusted once the canceller shows real echo suppression (the ERLE
gate), so these firings are themselves evidence the AEC was suppressing a
real acoustic echo. The quantitative acoustic ERLE number (replacing the
36.6 dB synthetic figure) remains a bench measurement to run under
controlled conditions; the *functional* claim — you can interrupt an AI
speaking from loudspeakers by talking over it — is now proven.

Session grand total: eleven deliberate barge-ins across headphone-gated
and open-loudspeaker paths, 7.2-43.0 ms.
