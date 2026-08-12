# Troubleshooting report — Buzz agent replies & BuzzTalk gateway audio

**Date:** 2026-08-11
**Machine:** Mac mini (Apple Silicon), Buzz Desktop 0.5.9, BuzzTalk gateway launcher (`c561b7b`)
**Outcome:** Both issues fully resolved. Chat agents reply reliably; voice gateway transcribes and speaks end-to-end.

Two separate incidents were diagnosed and fixed this day. They interacted (the second was
discovered while verifying the first), so both are documented here.

---

## Incident 1 — Agent acknowledges chat messages but never replies

### Symptom

Messages @mentioning the Coordinator in The Bridge channel received the acknowledgement
reaction but no reply. Two known-failed messages: "are you able to see my buzztalk repo on
this mac mini?" (9:00 AM CT) and "What day is it?" (2:06 PM CT). After the first fix
attempt, behavior became *intermittent* — one reply arrived (5:07 PM), the next message
(5:08 PM) again went unanswered.

### Investigation steps

1. **Located the agent runtime.** Buzz Desktop runs each agent as a `buzz-acp` harness
   process (cwd `~/.buzz`) which drives a `claude-agent-acp` (Claude Code) subprocess.
   Agent logs: `~/Library/Application Support/xyz.block.buzz.app/agents/logs/`, named
   `<agent-pubkey>__<team-id>.log`.
2. **Read the Coordinator's log at both failure timestamps.** Both showed the relay
   connection, subscription, agent-pool initialization (`agent_pool_ready agents=10`) at
   exactly the message times — then nothing. No error, no publish, no denial.
3. **Found the actual reply text.** The `claude-agent-acp` session transcripts
   (`~/.claude/projects/-Users-michaelrobinson--buzz/*.jsonl`) for both timestamps ended
   with complete, correct assistant answers ("Yeah, found it. `~/Code/buzztalk/` on this
   box…", "Today Aug 11, 2026.") that never appeared in chat.
4. **Confirmed non-delivery at the relay.** Queried the hosted relay directly
   (`BUZZ_RELAY_URL=https://mrtek.communities.buzz.xyz buzz messages get --channel <uuid>`):
   both user messages present; Coordinator's last published message was Aug 9, 7:50 PM.
   The replies never left the machine.
5. **Identified the publish mechanism.** A reply reaches the channel only when the agent
   itself executes `buzz messages send --channel <uuid> --reply-to <event-id>` during its
   turn (the harness prompt instructs exactly this). The working Aug 9 session transcript
   contains 95 such executions. Both failed Aug 11 sessions contain **zero** — the model
   wrote the answer as plain final text and ended the turn without running the send command.
6. **Diffed the working vs. failing configuration.** Working sessions (Aug 9–10) ran
   `claude-opus-5` (`model=opus[1m]` in the buzz-acp start line). Failing sessions ran
   `claude-sonnet-5`. The agent config (`managed-agents.json`) showed
   `"model": "sonnet"` with `updated_at` 8:56:55 AM CT — minutes before the first failure —
   coinciding with the first agent restart after Buzz Desktop auto-updated to 0.5.9
   (binary dated Aug 10, 5:54 PM).

### Ruled out

- **Harness permission block on the send command** (the documented Aug 9 gotcha requiring
  a `.claude/settings.local.json` allowlist): the harness runs
  `permission_mode=bypassPermissions` — no allowlist needed, nothing was blocked.
- **Owner/respond gating:** `respond_to=owner-only` resolved the correct owner pubkey; the
  messages were owner-authored and were delivered to the agent (sessions ran).
- **Relay/network failure:** the primary harness instance connected and stayed connected;
  the user's messages reached both the relay and the agent.
- **Auth failure:** sessions authenticated and produced completions normally.
- **Global Claude Code customizations (caveman-mode hooks etc.) as the cause:** these do
  leak into the agents' sessions (replies were visibly caveman-styled), but they were
  equally present in the working Aug 9 sessions (63 hook hits in a working transcript).
  Contamination is real and worth isolating eventually, but it was not the differentiator.

### Root cause

**The Buzz 0.5.9 update (or its first-launch migration) switched the Coordinator's model
from `opus[1m]` to `sonnet`. `claude-sonnet-5` follows the harness's
"reply by running `buzz messages send`" contract unreliably — it often answers as plain
final text and never executes the send, so the reply is generated but never published.**
The acknowledgement reaction fires on receipt, before any of this, which is why messages
looked "seen but ignored." The 5:07 PM success followed by the 5:08 PM failure on the same
model demonstrated the intermittency.

### Fix

- Set `"model": "opus[1m]"` on both Coordinator entries in
  `~/Library/Application Support/xyz.block.buzz.app/agents/managed-agents.json` and
  restarted the agent. Replies became reliable immediately.
- Enhancement while in there: per-agent `"env_vars": {"BUZZ_ACP_SUBSCRIBE": "all"}` on the
  Coordinator so it auto-responds to every owner message without an @mention
  (`respond_to=owner-only` still gates authors; Researcher/Scribe remain mention-only).

### Side findings

- A second, zombie Coordinator harness (from a stale second team) endlessly retries
  `ws://127.0.0.1:3000` — the local dev relay deleted by the Aug 10 reboot. Harmless but
  noisy; remove the stale team in Buzz when convenient.
- The `buzz` CLI in `~/.local/bin` defaults to `http://localhost:3000`; use
  `BUZZ_RELAY_URL=https://mrtek.communities.buzz.xyz` for the hosted community.

---

## Incident 2 — Voice gateway "Listening" but never hears speech

### Symptom

`buzztalk-gateway on` started the daemon successfully (connected, authenticated,
`[state] Listening`), but speech produced nothing in the logs — while macOS
Settings → Sound showed the microphone level meter moving.

### Investigation steps

1. **Verified daemon health:** correct flags (relay, The Bridge channel, Coordinator
   pubkey, `--vpio`, 700 ms endpointing), connected, authenticated, Listening.
2. **Checked the log for capture activity:** zero device events, zero partials, zero
   watchdog triggers — the daemon was receiving no usable audio at all.
3. **Checked per-app microphone permission (TCC):**
   `com.apple.Terminal → 0` (denied). The daemon inherits the mic permission of the app
   that launched it. macOS delivers **silent zeros** to a denied client — no error, no
   prompt, stream "works." Previous successful sessions had been launched from Solo
   (`com.soloterm.solo → 2`, allowed), which is why this had never bitten before.
4. **After granting permission — no change.** Because the daemon had *not actually been
   restarted*: same PID, started pre-grant. Permission is evaluated when the capture
   stream is created; the running process kept its denied verdict. A real restart fixed
   the "no frames at all" layer: VAD began triggering (`UserSpeaking`).
5. **New symptom: speech detected but empty transcripts** (`[final]` blank, daemon
   correctly refusing to publish empty content). `capture-dump` measured the WH-CH720N at
   peak 0.092 — energy, but no intelligible speech at the mic.
6. **Wrong turn (documented for honesty):** the presence of "Jump Desktop Audio" as the
   output device led to a remote-session hypothesis, and audio was rerouted to the Jump
   Desktop virtual devices. `capture-dump` then measured **exact digital silence
   (peak 0.000)** — those devices are installed by Jump's server component and produce
   nothing unless a remote session actively forwards audio. The user was physically at
   the Mac mini; this detour was reverted.
7. **Reverted to the headset and applied the physical checklist:** WH-CH720N as input and
   output, headset **worn** and **off its charge cable** (the WH-CH720N mic degrades to
   ~-54 dBFS while charging — a previously documented gotcha with its own watchdog fix).
8. **Result:** immediate streaming partials, near-perfect final transcript, publish,
   Opus Coordinator reply, TTS spoken in-ear. Full duplex confirmed.

### Ruled out

- **Broken daemon/launcher:** startup, relay auth, and state machine were all correct;
  the launcher's 72-check hardware-free audit had passed the day before.
- **Missing speech models:** `--model-status` showed STT and TTS models present (284 MB).
- **Broken headset:** the OS-level meter and later transcripts proved the hardware fine.
- **Jump Desktop / remote-session audio:** user was local; virtual devices were a red
  herring (and measurably silent).
- **The `--vpio` engine:** worked as designed once real audio reached it.

### Root causes (layered)

1. **macOS TCC microphone permission for the launching app** — Terminal was denied, so
   the daemon captured pure silence with no visible error. *This is per-launcher*: Solo,
   Terminal, and Shortcuts are separate TCC clients.
2. **A permission grant only takes effect on a genuine process restart** — the first
   "restart" hadn't actually happened, masking the fix.
3. **Physical mic constraints:** the Mac mini has **no built-in microphone** — the
   WH-CH720N is the machine's only real input — and its mic dies on the charge cable.

### Operator checklist (gateway silent but Listening)

1. Mic permission for the app that launched the daemon
   (System Settings → Privacy & Security → Microphone). Then genuinely restart:
   `buzztalk-gateway off && buzztalk-gateway on`. First launch from Shortcuts prompts once.
2. Headset worn, powered, **not charging**.
3. Input device really is the headset: `SwitchAudioSource -c -t input`
   (ignore "Jump Desktop Microphone"/"Jump Desktop Audio" unless deliberately remote).
4. Still silent → measure: stop the gateway, run `capture-dump`, speak during the 5-second
   window. Peak ≈ 0.000 = permission/routing; ≈ 0.09 = mic not near speech; > 0.3 = healthy.
5. Models present: `buzztalkd --model-status`.

---

## Final working configuration

- Coordinator: `opus[1m]`, `BUZZ_ACP_SUBSCRIBE=all`, `respond_to=owner-only`.
- Gateway: configured via `buzztalk-gateway configure`
  (config `~/.config/buzztalk/gateway.conf`), binary symlinked at
  `~/.local/bin/buzztalkd`, Shortcuts wrapper sets
  `BUZZTALKD_BIN="$HOME/Code/buzztalk/target/release/buzztalkd"` and runs
  `buzztalk-gateway on` (or `toggle`).
- Audio: WH-CH720N input + output, worn, off charger; Terminal (and, after first run,
  Shortcuts) granted microphone permission.
