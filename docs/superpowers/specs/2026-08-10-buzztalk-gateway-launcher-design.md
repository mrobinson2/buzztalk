# BuzzTalk Gateway Launcher Design

**Date:** 2026-08-10  
**Status:** Approved for planning  
**Scope:** macOS and Windows operator workflow for the standalone `buzztalkd` audio gateway

## Goal

Make the standalone BuzzTalk audio gateway usable without a Buzz Desktop button. After one guided setup, a person can start, stop, toggle, inspect, or follow the gateway with one short command.

macOS:

```bash
buzztalk-gateway configure
buzztalk-gateway on
buzztalk-gateway off
buzztalk-gateway toggle
buzztalk-gateway status
buzztalk-gateway logs
```

Windows PowerShell:

```powershell
buzztalk-gateway.ps1 configure
buzztalk-gateway.ps1 on
buzztalk-gateway.ps1 off
buzztalk-gateway.ps1 toggle
buzztalk-gateway.ps1 status
buzztalk-gateway.ps1 logs
```

The helper manages the existing `buzztalkd` process. It does not embed BuzzTalk into Buzz Desktop, change Buzz identity behavior, or create a new relay protocol.

## Chosen Approach

Ship two thin, platform-native launchers with the existing installers and release archives:

- `scripts/buzztalk-gateway` for macOS using POSIX shell.
- `scripts/buzztalk-gateway.ps1` for Windows using PowerShell.

This is preferred over repository-only scripts because normal installer users receive the helper. It is preferred over adding lifecycle subcommands to the Rust binary because the requested workflow does not require a new daemon API, IPC surface, or additional compiled executable.

Both launchers implement the same command names, configuration schema, lifecycle rules, messages, and exit-code contract. Platform-specific process APIs remain isolated in their respective scripts.

## User Workflow

### First use

`configure` prompts for:

1. Buzz relay URL.
2. Buzz channel UUID.
3. Agent public key.
4. Path to the user's existing signing-key file.
5. Headphone routing preference.
6. Endpoint-silence duration, defaulting to 700 ms.

On macOS, VoiceProcessingIO is enabled by default. On Windows, it is disabled because `--vpio` is macOS-only. Configuration ends by printing the config path and the exact `on` command.

Before writing the configuration, `configure` verifies that the provided signing-key path exists, refers to a regular file, and can be opened for reading by the current user. The check opens and immediately closes the file without reading, parsing, or displaying any bytes. A missing, non-file, or inaccessible path stops configuration with `Signing key file is missing or unreadable: <path>` and leaves any existing configuration unchanged.

The command refuses to overwrite an existing configuration until the user confirms. A non-interactive invocation never prompts unexpectedly; if stdin is not interactive and configuration is missing, the helper exits with the configuration error code and prints the required `configure` command.

### Daily use

- `on` starts `buzztalkd` in the background and returns to the terminal.
- `off` stops only the process previously started by this helper.
- `toggle` stops a running gateway or starts a stopped gateway.
- `status` reports `running`, `stopped`, `stale state`, or `startup failed` with the PID and log path when relevant.
- `logs` displays the last 50 lines and follows new output until interrupted.

`on` and `off` are idempotent. Calling `on` while the owned gateway is running or `off` while it is stopped succeeds and explains that no state change was necessary.

## Configuration

Default paths:

- macOS: `~/.config/buzztalk/gateway.conf`
- Windows: `%LOCALAPPDATA%\BuzzTalk\gateway.conf`

The format is a strict line-oriented `KEY=VALUE` file. Blank lines and lines beginning with `#` are ignored. Values are treated as literal text after the first `=`; neither launcher evaluates shell expressions, PowerShell expressions, command substitutions, or environment-variable references from the file.

Recognized keys are:

```text
RELAY=wss://community.communities.buzz.xyz
CHANNEL=00000000-0000-0000-0000-000000000000
AGENT_PUBKEY=npub1...
KEY_FILE=/path/to/buzztalk.key
HEADPHONES=true
ENDPOINT_SILENCE_MS=700
VPIO=true
```

Required keys are `RELAY`, `CHANNEL`, `AGENT_PUBKEY`, and `KEY_FILE`. Boolean values must be `true` or `false`. `ENDPOINT_SILENCE_MS` must be a positive integer. Unknown or duplicate keys are configuration errors rather than silently ignored input.

The config stores the path to the signing-key file, never the signing key. The macOS launcher creates the config and state directories with user-only permissions and writes files under `umask 077`. Windows uses directories inside the current user's `%LOCALAPPDATA%` profile and preserves the inherited per-user ACL. The README warns that gateway logs may contain transcribed conversation text even though they never contain the signing key.

## Command Construction

The launchers construct an argument array and invoke the binary directly. They do not concatenate or evaluate a command string.

The base command is:

```text
buzztalkd
  --relay <RELAY>
  --channel <CHANNEL>
  --agent-pubkey <AGENT_PUBKEY>
  --key-file <KEY_FILE>
  --endpoint-silence-ms <ENDPOINT_SILENCE_MS>
```

`--headphones` is added when `HEADPHONES=true`. `--vpio` is added only on macOS when `VPIO=true`. Windows rejects `VPIO=true` with a clear configuration error rather than passing an unsupported option.

The binary is resolved in this order:

1. `BUZZTALKD_BIN`, when set for tests or an explicit advanced override.
2. A `buzztalkd` or `buzztalkd.exe` sibling beside the installed launcher.
3. `buzztalkd` or `buzztalkd.exe` on `PATH`.

Failure to find an executable prints the expected installer location and exits without creating process state.

## Process and State Management

Default runtime paths:

- macOS state: `~/.local/state/buzztalk/`
- Windows state: `%LOCALAPPDATA%\BuzzTalk\state\`

The directory holds the PID record and protected gateway logs. The launchers append to these logs and never rotate or truncate them automatically. Logs are therefore unbounded and may contain transcripts; operators are responsible for rotation or cleanup. Tests may override config, state, binary, and startup timing through documented `BUZZTALK_GATEWAY_*` environment variables:

- `BUZZTALK_GATEWAY_CONFIG_DIR`
- `BUZZTALK_GATEWAY_STATE_DIR`
- `BUZZTALK_GATEWAY_STARTUP_WAIT_MS`

`BUZZTALK_GATEWAY_STARTUP_WAIT_MS` must be a positive base-10 integer and defaults to `1000`. It changes only the post-launch survival check; it does not add a health check or change gateway timeouts. `BUZZTALKD_BIN` remains the explicit binary-path override described above.

### Start transaction

`on` performs these steps in order:

1. Parse and validate configuration.
2. Resolve the `buzztalkd` executable.
3. Inspect any PID record and remove it only when it is stale.
4. Start `buzztalkd` in the background with stdout and stderr redirected to protected log files.
5. Record the PID, resolved executable path, and process start time atomically.
6. Wait for `BUZZTALK_GATEWAY_STARTUP_WAIT_MS` milliseconds (1000 by default) and verify that the same process is still alive.
7. Report `running` with the PID and log command.

There is no gateway health endpoint, so readiness means the launched process survived the startup window. If it exits during that window, the helper removes its PID record, reports `startup failed`, and prints the last relevant log lines.

### Ownership check

Before reporting a gateway as running or stopping it, the helper verifies both:

- The recorded PID exists.
- The process identity matches the executable identity stored at launch.

A missing process is stale state and the record is removed. A live PID belonging to another process is never signaled; the helper reports an ownership error and preserves enough state for manual diagnosis.

On Windows, process name alone is never sufficient. The launcher resolves the selected `buzztalkd.exe` to an absolute path before launch and records that path plus the process start time returned for the new process. For every later ownership check it obtains `ExecutablePath` and `CreationDate` for the PID through `Win32_Process`. The PID is owned only when:

1. The selected path is resolved at launch with `Resolve-Path -LiteralPath` and `[System.IO.Path]::GetFullPath()`. The observed `ExecutablePath` is normalized with `[System.IO.Path]::GetFullPath()`. The two absolute paths compare equal using `StringComparison.OrdinalIgnoreCase`.
2. The observed process creation time equals the recorded creation time.

If `ExecutablePath` or `CreationDate` cannot be read, normalization fails, or either comparison differs, ownership verification fails closed and `off` does not call `Stop-Process`. This rule prevents a process-name match, a PID reused by another executable, or a separately launched `buzztalkd.exe` at another path from being treated as owned.

### Stop transaction

`off` performs these steps:

1. Verify the PID and process identity.
2. Request normal termination.
3. Poll for up to five seconds.
4. Force termination only if the owned process remains alive.
5. Remove the PID record after the process is confirmed absent.
6. Report `stopped` and retain logs for diagnosis.

On macOS this uses `TERM`, followed by `KILL` only after the timeout. On Windows it uses `Stop-Process`, waits, and then retries with `-Force` only after the timeout.

### Exit codes

- `0`: requested action succeeded; `status` found the gateway running.
- `1`: startup, shutdown, or process-ownership failure.
- `2`: invalid command or invalid/missing configuration.
- `3`: `status` found the gateway stopped.

## Installer and Release Integration

The macOS release archive contains `buzztalk-gateway`; the Windows release archive contains `buzztalk-gateway.ps1`. Each installer stages and validates the helper alongside `buzztalkd` before replacing the installed set. A failed helper installation triggers the installer's existing rollback behavior so users never receive a mixed binary/script version.

The macOS installer marks the helper executable. The Windows installer places the PowerShell script beside `buzztalkd.exe`. Existing custom install-directory behavior remains authoritative.

The Windows README and installer completion message document an invocation-scoped fallback for machines whose PowerShell execution policy blocks the script:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\BuzzTalk\bin\buzztalk-gateway.ps1" status
```

The operator substitutes the desired launcher command for `status`. This bypass applies only to that PowerShell process; the installer does not change machine-wide or current-user execution policy. No `.cmd` companion is added in this change.

Linux is not part of this change. The POSIX implementation should avoid gratuitous macOS-only syntax, but no Linux packaging or support claim is added until its live audio path is validated.

## README Design

Add a prominent `Turn the audio gateway on or off` section near the beginning of the existing build-and-run instructions. It contains:

1. The one-time `configure` command for macOS and Windows.
2. The three daily commands: `on`, `off`, and `status`.
3. `toggle` and `logs` as optional conveniences.
4. The two default config locations.
5. A short explanation that this runs standalone beside Buzz until a Buzz UI control exists.
6. Recovery guidance for startup failure, stale state, and viewing logs.
7. A privacy and maintenance note that logs are unbounded, can contain transcripts, and must be rotated or removed by the operator when appropriate. Automatic rotation is not part of this change.
8. The invocation-scoped PowerShell execution-policy fallback, explicitly stating that it does not modify persistent policy.
9. A platform note: macOS defaults to the validated VoiceProcessingIO route; the Windows launcher is supported while live Windows audio validation retains the status documented elsewhere in the README.

The existing manual foreground `buzztalkd` example remains as an advanced/debugging path and links back to the launcher section.

## Error Messages

Messages lead with the outcome and end with the next action. Required cases include:

- `BuzzTalk gateway is already running (PID N).`
- `BuzzTalk gateway is stopped.`
- `Configuration not found. Run: buzztalk-gateway configure`
- `BuzzTalk failed during startup. Review: <log path>` followed by recent lines.
- `Refusing to stop PID N because it is not the BuzzTalk gateway started by this helper.`
- `VPIO=true is only supported on macOS; set VPIO=false on Windows.`

The scripts never echo configuration values containing a secret and never print key-file contents.

## Testing Strategy

Behavior is implemented test-first with fake gateway processes; tests never open an audio device or contact a relay.

Shared behavioral cases for shell and PowerShell are:

1. Missing configuration exits 2 and gives the configure command.
2. Invalid and duplicate keys are rejected.
3. `configure` rejects a missing, non-file, or unreadable key path without reading or displaying file contents.
4. `on` passes each configured value as one literal process argument, including paths containing spaces.
5. `on` records a live fake process and returns 0.
6. Repeated `on` is idempotent and does not create a second process.
7. `status` returns 0 while running and 3 while stopped.
8. `toggle` transitions stopped → running → stopped.
9. `off` terminates only the recorded owned process.
10. Windows rejects a process with the same name but a different full executable path.
11. Windows rejects a process with the same PID/path but a different recorded creation time.
12. A reused PID or wrong process identity is never terminated.
13. A stale PID record is cleaned safely.
14. Immediate child failure removes state, returns 1, and prints recent logs.
15. The default startup wait is 1000 ms, while `BUZZTALK_GATEWAY_STARTUP_WAIT_MS` shortens it in isolated tests.
16. Invalid startup-wait overrides are rejected as configuration errors.
17. macOS includes `--vpio` when enabled; Windows rejects it.
18. Key-file contents never appear in stdout, stderr, state, or logs created by the helper.
19. Paths and logs are created in overrideable temporary directories during tests.

Release-workflow and installer tests assert that the correct helper is packaged, staged, installed, and rolled back with its matching binaries. README command examples are smoke-tested against the launchers' help output where practical.

## Acceptance Criteria

- A newly installed macOS user can configure once and subsequently control the gateway with the six documented commands.
- A newly installed Windows user can do the same from PowerShell.
- The daily commands require no relay, channel, agent, or key arguments.
- No secret is stored in the gateway config.
- `configure` refuses a missing or unreadable signing-key file without reading its contents.
- Duplicate launches are prevented.
- Windows ownership requires an exact normalized full executable-path and process-start-time match; stale or reused PIDs cannot cause an unrelated process to be killed.
- Startup errors are visible without locating a hidden log manually.
- The startup survival wait defaults to one second and can be shortened through `BUZZTALK_GATEWAY_STARTUP_WAIT_MS` for tests.
- Installer rollback remains atomic across binaries and the new helper.
- Automated lifecycle tests pass on macOS and Windows CI without audio hardware.
- README and installer notes provide a per-invocation PowerShell execution-policy fallback without changing persistent policy.
- README states that transcript-bearing gateway logs grow without bound and require operator-managed cleanup or rotation.
- The README distinguishes launcher support from the current live-audio validation status of each platform.

## Non-Goals

- Adding the Buzz Desktop Conversation button.
- Starting or stopping Buzz Desktop itself.
- Running the gateway automatically at login or as a system service.
- Supporting multiple simultaneous gateway instances.
- Managing or generating signing keys.
- Discovering relay, channel, or agent identifiers automatically.
- Adding Linux packaging in this change.
- Adding a network health endpoint or IPC control API to `buzztalkd`.
