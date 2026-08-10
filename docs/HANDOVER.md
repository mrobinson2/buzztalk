# BuzzTalk — session handover

*Written 2026-08-09 for a fresh session to pick up cold. Read this first.*

## What BuzzTalk is

A full-duplex voice conversation layer for [Buzz](https://github.com/block/buzz)
(block/buzz), the team-chat platform where AI agents are first-class channel
members. You talk → speech transcribes locally → posts as a normal signed
chat message under your identity → an agent replies → the reply is spoken
aloud → and you can interrupt the agent by talking (barge-in). The engine
owns the audio device so capture and playback share one clock, which gives
the echo canceller a true reference — the thing Buzz's webview-capture
architecture can't do, and the whole reason this exists.

Repo: **https://github.com/mrobinson2/buzztalk** (owner `mrobinson2`).
Run commands from the repository root. Ensure the Rust toolchain's binary
directory is on PATH before invoking `cargo`.

## Where it stands (proven, this session)

All measured on real hardware against a private hosted Buzz community,
not a mock:

- **Conversation loop closed** — live Claude agents reply to spoken input,
  spoken back. Dozens of turns.
- **~18 barge-ins, 7.2–43 ms** voice-to-silence. 7.2 ms was over real
  loudspeakers with AEC active (launch-gate physics proven). Bluetooth
  runs ~20–40 ms (BT output latency).
- **VoiceProcessingIO engine** (`--vpio`, macOS): full-duplex on ONE
  Bluetooth headset — the config that was digital silence under the old
  two-stream engine. This is the key macOS win and the iOS foundation.
- **Self-healing audio**: a device watchdog (stream error / default-device
  or sample-rate change / stall) and a dead-capture watchdog (3 wordless
  speech turns → rebuild) both rebuild the engine in ~1 s. Survived many
  headset charge-disconnects live without manual restarts.
- **Voice-commanded agent crew** works: one spoken request → Coordinator
  (dispatcher) delegates to Researcher/Scribe → they act → Coordinator
  relays a spoken summary. See `docs/VOICE-CREW-SETUP.md`.
- **👀 "listening" reaction** on each spoken message (fires after the relay
  OKs the message — a race fix; see commit 4b66820).

## The live setup used for validation (historical)

- `buzztalkd` ran on an Apple Silicon Mac, signing as the test user in a
  private hosted channel and speaking replies from the Coordinator agent.
  Exact relay, channel, identity, agent, and temporary-log identifiers are
  intentionally omitted from this public handover.
- The validation used a session-provided signing credential. Its former
  location is intentionally omitted; rotate any credential ever exposed in
  a terminal or session transcript before reusing that identity.
- The three crew agents (Coordinator/Researcher/Scribe) are **app-run**
  registered Buzz agents in the private hosted community, Opus-backed,
  `respond_to=owner-only`, `subscribe=Mentions`. They live on the hosted
  relay, not the local development relay.
- A temporary Buzz clone supplied the local dev relay, `buzz-acp` harness,
  and `buzz` CLI used to query the relay. That disposable checkout was not
  a durable part of the repository and may no longer exist.

## Key running-it facts (learned the hard way)

- **`--vpio` is macOS-only** — full-duplex over Bluetooth. Omit it on
  Windows/Linux (uses the portable cpal engine).
- **Agents only see p-tagged messages** (`subscribe=Mentions`). `buzztalkd
  --agent-pubkey <pk>` p-tags every spoken message to that agent AND marks
  whose replies are spoken. A plain CLI `messages send` needs `--mention`.
- **Channel and agents must be on the same relay.** The single biggest
  time-sink this session was agents on the hosted community while a test
  channel sat on localhost — they never shared a room.
- **`--speak-all`** disables the user-directed speech filter. By default
  (`speak_only_user_directed=true`) only agent messages that p-tag the user
  are spoken, so a dispatcher's `@teammate` delegation is silent.
- **Headset charging kills its Bluetooth** — every "flap" this session was
  the user plugging the WH-CH720N in to charge, not flaky hardware. The
  self-healing handles it; don't misdiagnose it as a bug.
- macOS default devices flip constantly; `SwitchAudioSource -s "WH-CH720N"
  -t input/output` forces them back. The daemon's watchdog then rebuilds.

## Open work (backlog, roughly prioritized)

**Voice-polish bugs found live:**
1. **TTS output front-clipping** (FIX SHIPPED `0fd7828`, awaiting
   live-ear confirmation) — the *start* of spoken agent replies got cut.
   Root cause: playback underruns filled the device with exact digital
   zeros, letting downstream gates close (VPIO far-end processing, BT
   sink power-save mute); the gate re-opening swallowed speech onset.
   Fixed by ~-66 dBFS comfort-noise underrun fill in
   `next_playback_sample` (shared: cpal + VPIO + iOS). Daemon restarted
   on the fixed binary 2026-08-09 evening. Confirm on the BT headset,
   then close.
2. **Mic front-clip after silence** — first word after a quiet gap clips;
   partly mitigated (preroll 1500 ms) but the deep case is the device wake.
3. **Streaming agent replies** — replies arrive as one whole message then
   TTS speaks; streaming would cut the ~10 s "thinking" silence.
4. **Semantic endpointing** — reply sooner on a complete thought, wait on a
   trailing "…in". The turn-taking-feel differentiator.

**Reach:**
5. **iOS voice port** — STARTED, compiles (see `docs/IOS-VOICE-PORT.md`
   build-status section). Blocked on: full Xcode (this box has Command
   Line Tools only) + an iOS onnxruntime for `sherpa-onnx-sys`. `buzztalk-ffi`
   (C ABI) is scaffolded. Next: install Xcode, wire iOS onnxruntime, then
   the Flutter mic button.
6. **Windows/Linux audio** — untested. `docs/WINDOWS-TEST.md` has a
   ready-to-run Windows guide for a built-in mic + cpal (`--vpio` is
   macOS-only). A physical Windows run and captured terminal output are
   still required.
7. **Mic button in Buzz desktop** — real drop-in code +
   guide in `examples/buzz-desktop-integration/`. Landing it is a
   block/buzz PR (`docs/UPSTREAM-PROPOSAL.md` is the pitch).

**Ship:**
8. **The 15-second launch video** — the one gate before a Show HN. Record
   on the Mac: agent talking in-ear, user interrupts, instant silence,
   agent responds to the interruption. Everything needed is proven.

## Repo map (crates)

`buzztalk-core` (types/traits) · `buzztalk-audio` (cpal `DuplexEngine` +
`VoiceProcessingEngine`, device watchdog signature, route detection) ·
`buzztalk-aec` (sonora default; see `docs/PHASE-0.md` for the bake-off) ·
`buzztalk-vad` (endpoint + barge-in detectors) · `buzztalk-stt` (Parakeet
via sherpa-onnx) · `buzztalk-tts` (Kyutai Pocket) · `buzztalk-session`
(turn state machine, preroll) · `buzztalk-pipeline` (orchestrator;
`take_event_rx` for host pumps; dead-capture watchdog) · `buzztalk-buzz`
(relay transport, signing, eligibility, `build_reaction`) · `buzztalk-ffi`
(C ABI for mobile) · `buzztalk-labs` (measurement harnesses incl.
`capture-dump`). Binary: `buzztalkd`.

The live-session record reports **265 workspace + 18 audio** tests passing and a clean
clippy run at that point in time. Re-run `cargo test --workspace` for current evidence.

## Docs to read (all in `docs/`)

- `PHASE-0.md` — AEC bake-off + the closed-loop and live-session records.
- `live-session-2026-08-09/SESSION-REPORT.md` — privacy-safe aggregate
  measurements, validation setup, and observed bugs.
- `VOICE-CREW-SETUP.md` — reproduce the multi-agent voice demo.
- `IOS-VOICE-PORT.md` — the iPhone plan + what compiles.
- `UPSTREAM-PROPOSAL.md` — pitch to make BuzzTalk Buzz's voice engine.
- `WINDOWS-TEST.md` — Surface test steps.
- `README.md` — status table + Quick start.

## Commits this session (newest first)

`4b66820` reaction-after-OK fix · `e9d5944` Windows guide · `78b7fe7`
`take_event_rx` · `80bde39` desktop mic-button example · `af92dba` 👀
reaction · `885e77c` iOS VPIO port + FFI · `cbd15c5` user-directed speech
filter · `ec961fc` README quick-start + iOS plan · `fce7689` dead-capture
self-heal + guards · `42d7c6a` VPIO engine · `0a0e645` live-session docs ·
`2258e48` self-healing engine. All pushed to `origin/main`.

Before public commits, confirm that the repository's Git author name and
email are intentional rather than host-derived defaults.

## First moves for the next session

1. Read `SESSION-REPORT.md` and this file.
2. Check whether any local `buzztalkd`, Buzz relay, or harness processes
   from prior testing are still running.
3. Ask the user which backlog item — most likely the **TTS front-clip fix**
   (small, high-impact, user is hitting it) or the **launch video**.
