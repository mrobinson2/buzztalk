# BuzzTalk

<p align="center">
  <img src="docs/buzztalk-logo-light.png" alt="BuzzTalk bee wearing a headset and holding a radio" width="420">
</p>

**Natural voice conversations for [Buzz](https://github.com/block/buzz).**

Talk to your agents. Hear them respond. Interrupt them naturally.
Speech runs locally on your machine.

> **Status: alpha** — [v0.1.0-alpha.2](https://github.com/mrobinson2/buzztalk/releases/tag/v0.1.0-alpha.2).
> The full conversation loop, headset barge-in, and loudspeaker barge-in with live echo
> cancellation are validated on Apple Silicon macOS. The controlled real-world ERLE bench
> measurement and Windows/Linux audio validation remain open. See the
> [live session report](docs/live-session-2026-08-09/SESSION-REPORT.md).

## Where this actually is

| | |
|---|---|
| Echo cancellation | `sonora` chosen over `aec3` and `webrtc-audio-processing`. **36.6 dB** synthetic ERLE; live loudspeaker barge-in proves the functional acoustic path, while a controlled hardware ERLE number remains pending. |
| Duplex engine | 60 s soak: zero dropped frames, zero underruns, **no clock drift** between capture and the render reference. The macOS VPIO path also ran full-duplex on one Bluetooth headset. |
| Real-time budget | **481 µs** per frame in release against a 10 ms budget — ~20× headroom. (A debug build takes 7182 µs. Run voice in release.) |
| Render reference | Bit-exact with device output, silence included, enforced by test. |
| Barge-in gating | Fires on speech in 4 frames (**40 ms**). Rejects broadband coughs, keyboard clicks and door slams. |
| Barge-in → playback silent | **7.1–15.5 ms** synthetic; **7.2–43.0 ms** across live headphone and loudspeaker paths |
| End-of-speech → final transcript | **37–235 ms typical** live (outliers to ~680 ms on long utterances) |
| TTS warm synthesis | ~4× real time |
| CI | Formatting, clippy, and the workspace test suite run on macOS, Linux, and Windows; hardware-dependent tests stay ignored in CI. |

**Proven end to end against a real Buzz relay.** `buzztalkd` authenticated over NIP-42,
published a signed `kind:9` event with the correct `h` and `p` tags, and the message was
read back using Buzz's own CLI.

**The conversation loop is closed and used live (2026-08-09).** A live Claude agent —
upstream's `buzz-acp` harness running `claude-agent-acp` — replied to transcribed speech
over the relay, and `buzztalkd` spoke the reply aloud: speech in, `kind:9` out, real LLM
reply back, eligibility-checked, turn-attributed, synthesized, played. The same day, a
human on a real microphone held a multi-turn spoken conversation with the agent and
**interrupted it mid-sentence eight times, 19.5–43 ms from voice to silence** (Bluetooth
headset; the synthetic figure is 7–15 ms, cloud assistants sit near 700 ms). See
[`docs/PHASE-0.md`](docs/PHASE-0.md) and
[`docs/live-session-2026-08-09/SESSION-REPORT.md`](docs/live-session-2026-08-09/SESSION-REPORT.md).

**Loudspeaker barge-in: proven (2026-08-09, same day).** With the agent speaking from
real loudspeakers into an open room and echo cancellation live, the speaker deliberately
interrupted it mid-sentence three times: **7.2 / 33.1 / 39.6 ms** from voice to silence,
with no false self-interruptions. The barge-in gate only opens on evidence of real echo
suppression, so these firings double as proof the canceller worked against a live
acoustic echo. The engine also now **self-heals device changes**: stream errors,
default-device or sample-rate flips (Bluetooth renegotiation), and capture stalls trigger
an in-place rebuild within about a second — a headset power-cycle mid-conversation now
heals automatically.

**Still open:** the quantitative acoustic ERLE bench measurement (the 36.6 dB figure is
synthetic; the functional loudspeaker claim is not), confirmation of the latest TTS
front-clipping fix, and real audio-path validation on Windows and Linux. The macOS
VoiceProcessingIO path has already carried capture and playback on one Bluetooth headset.
See [`docs/HARDWARE-VALIDATION.md`](docs/HARDWARE-VALIDATION.md) and the
[session report](docs/live-session-2026-08-09/SESSION-REPORT.md).

---

## Quick start

BuzzTalk now has checksum-verifying release installers. Point the installed
binary at a Buzz relay and talk. Voice is validated on **macOS** (Apple
Silicon); Linux/Windows build and pass the offline suite, but their physical
audio paths remain unproven. Honest status lives in the table above.

**Prerequisites**

- A Buzz relay you can reach. Either a hosted community
  (`wss://<name>.communities.buzz.xyz`) or a local one — `git clone`
  [block/buzz](https://github.com/block/buzz) and `just relay` (needs
  Docker).
- Your Nostr signing key (the identity spoken messages are published as).
  From the Buzz desktop app it's in your OS keychain; or generate a fresh
  one for testing.
- Apple Silicon macOS for the validated audio path; a headset is recommended for the
  simplest first run. Loudspeaker barge-in is also validated with live AEC.

**1. Install BuzzTalk**

macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/mrobinson2/buzztalk/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/mrobinson2/buzztalk/main/install.ps1 | iex
```

| Installer target | Architecture | Release asset |
|---|---|---|
| macOS | Apple Silicon (`arm64`) | `buzztalk-macos-arm64.tar.gz` |
| Linux | `x86_64` | `buzztalk-linux-x86_64.tar.gz` |
| Windows | `x86_64` | `buzztalk-windows-x86_64.zip` |

The installer resolves the newest non-draft release (including prereleases),
selects the archive for the current platform, verifies its SHA-256 checksum,
stages and validates the matching executables plus platform launcher before
replacing any installed file, and restores the previous set if a replacement
fails. It never requests administrator
access, reads a signing key, or downloads speech models. Set `BUZZTALK_VERSION`
to pin a release and `BUZZTALK_INSTALL_DIR` to override the default
(`~/.local/bin` on macOS/Linux or `%LOCALAPPDATA%\BuzzTalk\bin` on Windows).
Unsupported operating-system/architecture combinations stop with a diagnostic.
Review the installer scripts before piping them to a shell if that better fits
your security policy.

## Turn the audio gateway on or off

The installed gateway helper is the standalone CLI/operator control for the
`buzztalkd` process. It is separate from the in-process Desktop Audio Bridge;
neither capability launches, replaces, or serves as a fallback for the other.

Configure once with the existing signing-key file. The helper checks that the
file is readable without reading or displaying its contents:

macOS:

```bash
buzztalk-gateway configure
```

Windows PowerShell:

```powershell
& "$env:LOCALAPPDATA\BuzzTalk\bin\buzztalk-gateway.ps1" configure
```

The daily commands need no relay, channel, agent, or key arguments:

```text
buzztalk-gateway on       # start the installed gateway
buzztalk-gateway off      # stop only the process this helper owns
buzztalk-gateway status   # running, stopped, or stale state
buzztalk-gateway toggle   # switch between on and off
buzztalk-gateway logs     # explicitly view and follow gateway logs
```

On Windows, substitute the PowerShell script path for each command. If the
machine's execution policy blocks the script, use this invocation-scoped
fallback; it does not change current-user or machine policy:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\BuzzTalk\bin\buzztalk-gateway.ps1" status
```

The macOS config is `~/.config/buzztalk/gateway.conf`; Windows uses
`%LOCALAPPDATA%\BuzzTalk\gateway.conf`. Runtime state is under
`~/.local/state/buzztalk/` on macOS and `%LOCALAPPDATA%\BuzzTalk\state\` on
Windows. Startup failures direct operators to the explicit `logs` command
without printing recent log lines; stale state is safe to inspect with
`status` before recovery.

Gateway logs are unbounded and may contain transcribed conversation text.
Automatic rotation and truncation are intentionally not part of the helper.
Operators must apply their own retention policy and rotate or remove logs when
appropriate. Launcher-generated status and error messages do not contain key
material, transcripts, channel identifiers, relay URLs, or resolved executable
paths.

The macOS helper defaults to its configured VoiceProcessingIO route. The
Windows helper provides process management only. The separate Desktop Audio
Bridge remains an Apple Silicon macOS-only capability and makes no Windows
claim.

**2. Fetch the speech models** (~285 MB, one time)

```bash
~/.local/bin/buzztalkd --download-models   # Parakeet STT + Kyutai TTS
~/.local/bin/buzztalkd --model-status       # verify
```

On Windows, use
`& "$env:LOCALAPPDATA\BuzzTalk\bin\buzztalkd.exe" --download-models`.

To build the default `sonora` AEC workspace instead, install Rust 1.91+ and run
`cargo build --locked --release -p buzztalk-buzz --bin buzztalkd`.

**3. Put your signing key in a file** (read once, never logged)

```bash
umask 077; printf %s 'nsec1…' > ~/buzztalk.key   # or 64-char hex
```

**4. Talk to an agent**

You need a channel your key is a member of, and at least one Buzz agent in
it (its pubkey is the `--agent-pubkey`). Then:

```bash
~/.local/bin/buzztalkd \
  --relay wss://<your-community>.communities.buzz.xyz \
  --channel <channel-uuid> \
  --agent-pubkey <agent-pubkey> \
  --key-file ~/buzztalk.key \
  --vpio \
  --headphones \
  --endpoint-silence-ms 700
```

Speak. Your words are transcribed locally, published as a signed message
from your identity, the agent replies, and the reply is spoken back —
interrupt it any time by talking. `--vpio` is macOS-only; omit it to use
the portable two-stream engine (but a Bluetooth headset's mic may get
starved — see the session report). `--agent-pubkey` both p-tags outgoing
messages and selects which replies are spoken. Drop `--headphones` for
loudspeakers with live AEC. A local relay can use `ws://localhost:3000`.

**Want the multi-agent "voice-commanded crew" demo** (speak, and a team of
agents divides the work)? That setup — roles, mention routing, one narrator
voice — is its own guide: [`docs/VOICE-CREW-SETUP.md`](docs/VOICE-CREW-SETUP.md).

**Installation is turnkey; first-time configuration is not yet.** There is no
mic button inside Buzz yet, so relay, channel, agent, and key configuration still
use command-line flags. The in-app setup is the next integration milestone
([`docs/UPSTREAM-PROPOSAL.md`](docs/UPSTREAM-PROPOSAL.md) for the in-Buzz
button; [`docs/IOS-VOICE-PORT.md`](docs/IOS-VOICE-PORT.md) for the phone).

---

## Why this exists

Buzz already has excellent local speech. It ships NVIDIA Parakeet TDT-CTC 110M for
recognition via sherpa-onnx, Kyutai Pocket TTS with nine bundled voices, voice-activity
detection, push-to-talk, per-agent voice assignment, hash-pinned model downloads, and an
Opus/NetEQ conferencing transport. It even has a cancellation path that silences playback
about 15 ms after a flag is set, mid-sentence.

What it does not have is the ability to **interrupt an agent by speaking**.

The reason is architectural, not a missing feature. Buzz captures the microphone in the
webview (`getUserMedia`) and plays synthesized speech from the Rust process (`rodio`).
Two audio clients, no shared reference signal — so the echo canceller has nothing to
subtract, and the only safe policy is to discard every microphone frame while the agent
is talking. Buzz does exactly that, and it is the correct call given the constraint.

BuzzTalk removes the constraint. It takes ownership of the audio device so capture and
playback live in one process with one clock, which means the exact samples sent to the
speaker are available as an echo-cancellation reference. That single change is what turns
Buzz's existing engines into a conversation.

**BuzzTalk is a conversation layer, not another speech stack.** It does not reimplement
recognition or synthesis. It owns the microphone, the speaker, the turn, and the
interruption.

## Architecture

```
cpal duplex engine (one process, one clock)
  ├─ capture ──► AEC ◄── render reference (the exact output samples)
  │               ├─► barge-in detector (strict, ERLE- and route-gated)
  │               ├─► pre-roll ring (500 ms, so the first word survives)
  │               └─► endpoint detector ──► speech recognition
  └─ output  ◄── speech synthesis
```

| Crate | Role |
|---|---|
| `buzztalk-core` | Types and traits. Zero I/O, zero engine dependencies. |
| `buzztalk-audio` | cpal duplex engine, lock-free rings, render-reference tap, output-route detection. |
| `buzztalk-aec` | `EchoCanceller` implementations, feature-gated, plus a null fallback. |
| `buzztalk-vad` | Endpoint and barge-in detectors, ERLE- and route-gated. |
| `buzztalk-labs` | Measurement harnesses. Not shipped; exists to produce evidence. |

The workspace also includes `buzztalk-stt`, `buzztalk-tts`, `buzztalk-session`,
`buzztalk-pipeline`, `buzztalk-buzz`, `buzztalk-models`, and the mobile FFI scaffold.

## Design rules

- `buzztalk-core` never imports an engine or an I/O crate. If it does, the abstraction has broken.
- Audio callbacks allocate nothing, lock nothing, log nothing.
- Backends are Cargo features. Users compile out what they do not want.
- Every failure degrades to something usable. Losing echo cancellation must not lose voice;
  losing voice must not lose Buzz.
- Interruption latency is a tested number, not an impression. A regression there is a build failure.

## Privacy

Audio processing is local. No network in the audio path. No raw audio is written to disk —
ring buffers only, in memory. Transcripts become ordinary Buzz messages with ordinary Buzz
retention; BuzzTalk keeps no separate transcript store. The interactive terminal does show
final transcripts and agent replies for operator feedback, so preserve or share captured
terminal logs only under the same policy as the corresponding Buzz channel.

## Platforms

| | CI matrix target | Audio validated on real hardware |
|---|---|---|
| macOS (Apple Silicon) | yes | **yes** — VPIO Bluetooth headset and open-loudspeaker AEC paths |
| Windows | yes | **no** |
| Linux | yes | **no** |

The CI matrix is configured to check formatting, clippy, and the workspace suite on all
three platforms. That does **not** prove voice works everywhere: no CI runner has an audio
device, so hardware paths sit behind `#[ignore]`. Route detection is implemented for macOS
only and returns `Unknown` elsewhere, which degrades safely to assuming an echo path.

## Licence

Apache-2.0, matching Buzz. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE) for third-party
attribution, including the CC-BY-4.0 obligations that travel with the speech models.
