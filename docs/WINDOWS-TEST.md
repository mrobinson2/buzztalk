# Testing BuzzTalk voice on Windows (built-in mic)

First-ever run of the non-macOS audio path. The Surface's built-in mic is
the ideal case: it's a wired device, so the Bluetooth-mic-starvation
problem that forced VoiceProcessingIO on macOS doesn't apply — the portable
cpal `DuplexEngine` should just work. No `--vpio` on Windows.

## One-time setup

1. **Visual Studio Build Tools** (the C++ workload) — the Rust MSVC
   toolchain and native deps (`ring`, onnxruntime) need a C/C++ compiler.
   Install "Desktop development with C++" from the VS Build Tools installer.

2. **Rust** — https://rustup.rs, take the default (MSVC) toolchain. Open a
   fresh terminal after so `cargo` is on PATH.

3. **Clone and build** (release — debug audio is far too slow):
   ```
   git clone https://github.com/mrobinson2/buzztalk
   cd buzztalk
   cargo build --release -p buzztalk-buzz --bin buzztalkd
   ```
   First build pulls onnxruntime for the STT engine; give it a few minutes.

4. **Fetch the speech models** (~285 MB, one time):
   ```
   .\target\release\buzztalkd.exe --download-models
   .\target\release\buzztalkd.exe --model-status
   ```

5. **Your signing key** — the same identity as your Buzz app (so voice
   posts as you). Put your `nsec1…` in a file, e.g. `key.txt`, in the repo
   folder. Keep it private; delete when done.

## Run

Supply a relay, channel, and Coordinator agent in the same community. The
private values used during the macOS validation session are intentionally
not published here:

```
.\target\release\buzztalkd.exe ^
  --relay wss://<your-community>.communities.buzz.xyz ^
  --channel <channel-uuid> ^
  --agent-pubkey <coordinator-pubkey> ^
  --key-file key.txt ^
  --endpoint-silence-ms 700
```

(No `--vpio`. Add `--headphones` if you use headphones instead of the
laptop speakers.) Windows will prompt for microphone permission the first
time — allow it. Then speak.

**Stop the Mac's `buzztalkd` first** — two daemons signing as the same
identity at once will both publish. One voice terminal at a time.

## What to watch — all first-time-on-Windows unknowns

- **Does capture work?** You should see `[state] UserSpeaking` then
  `[final] <your words>` in the terminal, and the message appears in the
  channel with Coordinator replying. If you get only `[final]` (empty) or
  fragments, cpal isn't delivering clean 48 kHz frames — capture that
  output.
- **Barge-in.** Talk over the spoken reply. On Windows, route detection
  returns `Unknown`, so the gate assumes an acoustic echo path and relies
  on the AEC (`sonora`, only ever validated on macOS audio). Watch the
  `barge-in -> playback silent` metric, and whether the agent's own voice
  ever self-triggers an interruption (that would mean the AEC needs Windows
  tuning).
- **Latency.** The `end-of-speech -> final transcript` number — should be
  in the same low-hundreds-of-ms range as macOS.

## Reporting back

Copy the terminal lines (`[state]`, `[final]`, `[agent]`, `[metrics]`, any
`error`). That tells us whether the cpal path works as-is on Windows or
needs platform-audio work — the one genuine unknown for "runs on any PC."
