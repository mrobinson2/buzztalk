# Solo ToDo #53 — Acceptance-Validation Results
## "BuzzTalk Gateway: complete hardware-free launcher acceptance validation"

Executed 2026-08-10 against the uncommitted working tree at `~/Code/buzztalk`.
No file in `~/Code/buzztalk` was modified, deleted, or `git add`-ed. One new
file was created: `tests/gateway-acceptance-extra.sh` (12 new hardware-free
tests, distinct name from the existing suites). No real `buzztalkd`, relay,
or audio device was ever touched — every test uses a fake shell script or a
compiled no-op `.exe` as the "gateway" process, per the design spec's own
testing strategy.

**Disk:** stayed at ~140–168Mi free for most of the session (started ~168Mi
free); ended at ~1.1Gi free after something outside this task's control
freed space. No cargo/npm/pnpm/model-download activity from this task. All
temp fixtures used `mktemp -d` under `$TMPDIR` and were cleaned up by each
script's own trap.

**Mid-session update:** partway through this audit, `scripts/buzztalk-gateway`
was updated in the working tree by the other (still-active) agent — not by
this audit. The fix landed for exactly the bug this audit's new tests found
(see below). All results in this document reflect the **current, patched**
state of the launcher, re-verified after the update. The bug is preserved
below as a record of what was found and how, with its status marked FIXED.

**pwsh:** not installed anywhere on this host (`which pwsh/powershell` all
report "not found"), and none could be installed given the disk constraint.
All Windows rows that require actually *running* PowerShell are marked
`BLOCKED: requires Windows CI`. Windows rows that only require reading a
literal constant string or file-placement logic are marked `PASS` from
direct source review, called out explicitly as source-only evidence.

---

## Headline finding: a real bug, found by the new tests — now FIXED

**Status: FIXED during this session** (by the other agent working the same
tree, not by this audit — this audit only ever reads/runs, never edits
`scripts/*`). Re-verified after the fix landed: all suites are green, and a
direct ad hoc repro of the exact failing case now shows the message
reaching stderr correctly. Preserved below as it was found, for the record.

**`scripts/buzztalk-gateway`, `validate_key_file()` (lines 34–45):**

```sh
if ! exec 3<"$key_file" 2>/dev/null; then
    return 1
fi
exec 3<&-
```

`exec 3<"$key_file" 2>/dev/null` is a **bare `exec`** (no command word), so
in POSIX shell its redirections are **applied permanently to the running
shell**, not scoped to that one statement. The `3<file` part is intentional
(open-then-close to prove readability without reading bytes — that part
works correctly). But `2>/dev/null` is *also* on that bare `exec`, so the
moment a configured key file is valid, **the script's own stderr is
silently redirected to `/dev/null` for the rest of that process's
lifetime.**

Every `fail()` call written *after* the successful key check in the same
invocation — i.e. every normal `on` invocation with a valid config — has
its message silently discarded. Only the exit code survives. Reproduced
twice, independently, with plain commands (no test framework):

```
$ env -u BUZZTALKD_BIN BUZZTALK_GATEWAY_CONFIG_DIR=... BUZZTALK_GATEWAY_STATE_DIR=... \
    sh scripts/buzztalk-gateway on
EXIT=1
stdout: (empty)
stderr: (empty)          # should have been:
                          # "BuzzTalk gateway executable was not found. Reinstall BuzzTalk or set BUZZTALKD_BIN."
```

```
$ BUZZTALKD_BIN=<fake binary that exits 1 immediately> ... sh scripts/buzztalk-gateway on
EXIT=1
stdout: (empty)
stderr: (empty)          # should have been:
                          # "BuzzTalk failed during startup. Run: buzztalk-gateway logs"
```

State cleanup and the exit code are both correct in these cases — only the
*message* is lost. This directly breaks:

- **AC8** ("Startup errors are visible without locating a hidden log
  manually") — in the common case (valid config), they are not: only a
  bare exit code reaches the terminal.
- **Testing Strategy #14** ("Immediate child failure ... points to the
  explicit `logs` command") — it does not, in real invocations.
- The **Error Messages** required cases `BuzzTalk failed during startup.
  Run: buzztalk-gateway logs` and `BuzzTalk gateway executable was not
  found. Reinstall BuzzTalk or set BUZZTALKD_BIN.` — both are correctly
  *coded* but unreachable once a key file validates.

**Scope of the bug:** only the shell (macOS) launcher. The PowerShell
`Fail()` function (`[Console]::Error.WriteLine(...)`) uses no equivalent
file-descriptor redirection, so source review shows Windows is very
unlikely to share this defect — but that is source review only, not an
executed confirmation (no pwsh available).

**Fix applied (externally, mid-session):** `validate_key_file()` now reads:

```sh
if (exec 3<"$key_file") 2>/dev/null; then
    return 0
fi
return 1
```

The `exec` is now inside a subshell `(...)`, so its redirections — including
`2>/dev/null` — are scoped to that subshell and discarded when it exits,
instead of leaking into the parent launcher process's own stderr. Re-run of
the exact repro command that showed the bug:

```
$ env -u BUZZTALKD_BIN BUZZTALK_GATEWAY_CONFIG_DIR=... BUZZTALK_GATEWAY_STATE_DIR=... \
    sh scripts/buzztalk-gateway on
EXIT=1
stderr: BuzzTalk gateway executable was not found. Reinstall BuzzTalk or set BUZZTALKD_BIN.
```

Message now delivered correctly. `tests/gateway-acceptance-extra.sh` is
12/12 green (was 10/12 before the fix).

---

## Suites run

| Suite | Command | Exit | Result |
|---|---|---|---|
| macOS gateway lifecycle | `sh tests/gateway.sh` | 0 | 6 passed; 0 failed |
| Installers (macOS host) | `sh tests/installers.sh` | 0 | 7 passed; 0 failed |
| **New:** acceptance-gap coverage | `sh tests/gateway-acceptance-extra.sh` | 0 (was 1 before the BUG-1 fix landed) | **12 passed; 0 failed** (2 failed pre-fix on the BUG-1 finding — see below; re-run clean after the fix) |
| Windows gateway lifecycle | `tests/gateway.ps1` | — | **not run** — no `pwsh`/`powershell` on this host, none installable (disk) |
| Windows installer | `tests/installers.ps1` | — | **not run** — same reason |

`tests/gateway-acceptance-extra.sh` was created new (12 tests) to close
matrix gaps the ToDo called out by name: stale-PID cleanup, reused/wrong-
identity PID safety, immediate startup-failure cleanup, `toggle` cycling,
default 1000 ms startup wait, non-interactive `configure`, overwrite
confirmation (via a real pty through Python's `pty` module — a plain pipe
does not exercise this path, see the "Non-interactive/overwrite" section
below), unknown/duplicate config keys, binary-not-found path safety, the
exit-code contract for an invalid command, a filesystem-level secret-leak
scan, and a README/`help` command-surface smoke check.

Before the mid-session fix (preserved for the record):

```
$ sh tests/gateway-acceptance-extra.sh
ok - stale PID record is cleaned safely (Testing Strategy #13)
ok - ownership mismatch is never signaled (Testing Strategy #12)
not ok - immediate startup failure cleans up state, no log echo (Testing Strategy #14)   <- BUG-1
ok - toggle cycles stopped -> running -> stopped (Testing Strategy #8)
ok - default startup wait is about 1000ms (AC9 / Testing Strategy #15)
ok - non-interactive configure with missing config does not prompt
ok - configure refuses overwrite without confirmation
ok - config rejects unknown and duplicate keys (Testing Strategy #2)
not ok - binary-not-found path omits resolved path, no state (Command Construction)      <- BUG-1
ok - invalid command exits 2 (Exit codes)
ok - no secret material written to state/log files (Testing Strategy #18/#20)
ok - README six commands all recognized by launcher help
10 passed; 2 failed
```

After the fix (current state — final result used for this audit's totals):

```
$ sh tests/gateway-acceptance-extra.sh
ok - stale PID record is cleaned safely (Testing Strategy #13)
ok - ownership mismatch is never signaled (Testing Strategy #12)
ok - immediate startup failure cleans up state, no log echo (Testing Strategy #14)
ok - toggle cycles stopped -> running -> stopped (Testing Strategy #8)
ok - default startup wait is about 1000ms (AC9 / Testing Strategy #15)
ok - non-interactive configure with missing config does not prompt
ok - configure refuses overwrite without confirmation
ok - config rejects unknown and duplicate keys (Testing Strategy #2)
ok - binary-not-found path omits resolved path, no state (Command Construction)
ok - invalid command exits 2 (Exit codes)
ok - no secret material written to state/log files (Testing Strategy #18/#20)
ok - README six commands all recognized by launcher help
12 passed; 0 failed
```

A number of additional spec scenarios were verified by direct ad hoc
commands (not folded into the permanent suite, to keep the new file's scope
tight) — each is cited by row below with its command and result: `logs`
command smoke test (both the "no log yet" and "follow existing content"
cases), the 5-second TERM→KILL stop-transaction fallback, `off` while
already stopped, `configure`'s own key-file check for missing/non-file
(directory)/unreadable (chmod 000) paths via a real pty, and a couple of
grep sweeps for Non-Goal boundary violations.

---

## Section 1 — Acceptance Criteria (17 rows)

| # | Status | Evidence |
|---|---|---|
| AC1 macOS first-use → daily commands | **PASS** | Composite: `configure` (pty-driven, see Non-interactive section), `on`/`off`/`status` (`tests/gateway.sh` lifecycle test), `toggle` (new suite), `logs` (ad hoc: ran `buzztalk-gateway logs` against a live fake process, backgrounded 0.5s, captured the pre-existing log line, then killed it cleanly) |
| AC2 Windows first-use → daily commands | **BLOCKED: requires Windows CI** | `tests/gateway.ps1` exists and is structurally sound (source-reviewed) but no `pwsh` on this host |
| AC3 daily commands take no relay/channel/agent/key args | **PASS** | Source: `case "$command" in on) on_gateway ;; ...` and PS `switch ($command)` — neither reads `$2..$n` |
| AC4 no secret stored in config | **PASS** | Config schema only ever stores `KEY_FILE=<path>`; new suite's secret-scan test confirms the literal key bytes never land in state/log files |
| AC5 `configure` refuses missing/unreadable key without reading it | **PASS** | `tests/gateway.sh: test_key_validation_never_reads_contents`; ad hoc pty run of `configure` itself with a missing path (exit 2, correct message, no config written); ad hoc directory-as-path and chmod-000 sub-cases (both exit 2, correct message) |
| AC6 duplicate launches prevented | **PASS** | `tests/gateway.sh: test_lifecycle_is_idempotent_and_status_exit_codes_are_stable` |
| AC7 Windows exact path+creation-time ownership | **BLOCKED: requires Windows CI** | `Test-OwnedProcess`/`Get-ProcessIdentity` in `scripts/buzztalk-gateway.ps1` (lines 124–144) implement exactly the spec's two-part check and fail closed if `Win32_Process` throws; not executed |
| AC8 startup errors visible without a hidden log | **PASS** (was FAIL, fixed mid-session) | See "Headline finding" above — the bare-`exec` stderr bug was found by this audit's new tests and fixed in the working tree during this session; re-verified clean |
| AC9 startup wait defaults to 1 s, shortened via env var | **PASS** | New suite: `test_default_startup_wait_is_about_1000ms` (measured ≥900 ms with no override); `tests/gateway.sh: test_startup_wait_override_is_validated` |
| AC10 installer rollback atomic across binaries+helper | **PASS (macOS)** | `tests/installers.sh: test_second_replacement_failure_rolls_back_first_binary`, `test_signal_after_first_replacement_restores_both_binaries`; Windows: `install.ps1`'s try/finally mirrors the same backup-then-replace-then-rollback shape (source-reviewed, not executed) |
| AC11 automated lifecycle tests pass on macOS+Windows CI w/o audio | **BLOCKED: requires Windows CI** | macOS half fully green (see suite table); the Windows half of this joint claim cannot be confirmed without an actual Windows CI run |
| AC12 README PS execution-policy fallback, invocation-scoped | **PASS** | README.md:147-153, explicit: "it does not change current-user or machine policy" |
| AC13 README unbounded log-rotation notice | **PASS** | README.md:162-164 |
| AC14 README distinguishes launcher support vs. audio-validation status | **PASS** | README.md:169-172 + Platforms table (README.md:293-299) |
| AC15 launcher independent of Desktop Audio Bridge | **PASS** | `grep -ni "desktop\|tauri\|huddle"` over both launcher scripts: no matches |
| AC16 Windows launcher makes no native-audio claim | **PASS** | README.md:170: "The Windows helper provides process management only." |
| AC17 ownership tests pass w/o hardware/relay/desktop | **BLOCKED: requires Windows CI** | macOS half PASS (new suite uses only fake shell processes); Windows half of the joint claim unconfirmed |

## Section 2 — Non-Goals (8 rows) — all PASS

| # | Status | Evidence |
|---|---|---|
| NG1 no Desktop Conversation button added | **PASS** | `git diff --stat` shows only scripts/tests/installers/README/release.yml changed — no UI files |
| NG2 never starts/stops Buzz Desktop | **PASS** | grep sweep, no "desktop/tauri/huddle" references in either launcher; launcher only ever signals its own recorded PID |
| NG3 no login-item/service registration | **PASS** | `grep -ni "launchctl\|systemd\|schtasks\|registry\|LaunchAgent\|LaunchDaemon\|New-Service\|Register-ScheduledTask"` — no matches in any changed file |
| NG4 no multiple simultaneous instances | **PASS** | Single `STATE_FILE`/PID model; `tests/gateway.sh` lifecycle test proves a second `on` does not spawn a new process |
| NG5 no key management/generation | **PASS** | `grep -ni "genkey\|keygen\|generate.*key\|openssl"` — no matches; `configure` only accepts an existing file path |
| NG6 no auto-discovery of relay/channel/agent | **PASS** | `grep -ni "curl\|Invoke-WebRequest\|Invoke-RestMethod\|wget"` inside the launchers — no matches; all four required keys are plain interactive prompts |
| NG7 no Linux packaging added | **PASS** | `git diff .github/workflows/release.yml install.sh \| grep -i linux` — no output; gateway only appended to macOS/Windows archive targets |
| NG8 no health endpoint/IPC surface added | **PASS** | `grep -ni "listen\|socket\|bind.*port\|http.*server\|named pipe\|ipc"` — no matches |

## Section 3 — Granular behavior coverage (55 rows)

### Key-file missing/unreadable (4 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| missing / non-file / unreadable path rejected | **PASS** | `tests/gateway.sh: test_key_validation_never_reads_contents` (missing, via `on`); ad hoc pty run of `configure` itself (missing, exit 2); ad hoc directory-as-path (exit 2, same message); ad hoc `chmod 000` (exit 2, same message) — all four executed directly |
| opens/closes without reading, parsing, or displaying bytes | **PASS** | Source review: `exec 3<"$key_file"` / `[IO.File]::Open(...)` then immediate close/dispose — no `cat`, `read`, or `Get-Content` call anywhere near the check |
| key-failure leaves existing config untouched | **PASS** | Ad hoc: config dir had zero entries after a failed `configure` run against a missing key |
| error omits the configured key path (TS#21) | **PASS** | Every failed-validation run above printed the fixed message with no path substring; `tests/gateway.sh` explicitly asserts the missing path string is absent from stderr |

### Literal argument handling (3 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| `on` passes each value as one literal arg incl. spaces | **PASS** | `tests/gateway.sh: test_on_passes_literal_arguments_and_hides_sensitive_values` — relay URL with an embedded space and key path with an embedded space both survive as single args |
| arg array built, no shell string eval | **PASS** | Source: `set -- --relay "$RELAY" ...` (macOS) / `$arguments = @(...)` + `Start-Process -ArgumentList` (Windows) — neither builds or evaluates a command string; `grep -n "eval\|Invoke-Expression"` over both scripts: no matches |
| config values never expanded/evaluated | **PASS** | Same grep sweep; config parser only ever does string splitting on the first `=` |

### Duplicate launch prevention (3 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| repeated `on` idempotent, no second process | **PASS** | `tests/gateway.sh: test_lifecycle_is_idempotent_and_status_exit_codes_are_stable` |
| `on` while running reports no-state-change | **PASS** | Same test — second `on` call output contains "already running" |
| `off` while stopped reports no-state-change | **PASS** | Ad hoc: `off` with no state file → "BuzzTalk gateway is already stopped." exit 0 |

### Stale / reused PID safety (4 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| stale PID record cleaned safely | **PASS** | New suite: `test_stale_pid_record_is_cleaned_safely` — killed the recorded PID directly, then `status` reported "stale state" and removed the record |
| `on` removes stale record before launching | **PASS** | Same test — a subsequent `on` after the stale-cleanup succeeded and started a fresh process |
| reused PID / wrong identity never terminated | **PASS** | New suite: `test_ownership_mismatch_never_signaled` — corrupted only the recorded start time on a still-alive owned process; `off` refused to signal it and reported the ownership error |
| ownership mismatch preserves state, no path leak | **PASS** | Same test — asserted the process was still alive after the refused `off`, and the error text contains no path |

### Windows full-path + creation-time ownership (7 rows) — all BLOCKED

| Criterion | Status | Evidence |
|---|---|---|
| process name alone never sufficient | **BLOCKED: requires Windows CI** | `Get-ProcessIdentity`/`Test-OwnedProcess` (buzztalk-gateway.ps1:124-144) always compares full path + creation time, never name alone (source-reviewed) |
| path comparison via `Resolve-Path`/`GetFullPath`, OrdinalIgnoreCase | **BLOCKED: requires Windows CI** | Lines 97-98, 128, 141 implement this exactly (source-reviewed) |
| creation time must match recorded value | **BLOCKED: requires Windows CI** | Line 142 (source-reviewed) |
| rejects same name, different path | **BLOCKED: requires Windows CI** | `tests/gateway.ps1` test 3 exercises this scenario; not executed (no pwsh) |
| rejects same PID/path, different creation time | **BLOCKED: requires Windows CI** | Same test 3 |
| unreadable identity fails closed, no `Stop-Process` | **BLOCKED: requires Windows CI** | `Get-ProcessIdentity`'s `catch { return $null }` (line 131-133) propagates to `Test-OwnedProcess` returning `$false`, which routes `Stop-Gateway` to the `Fail` branch rather than `Stop-Process` (source-reviewed) |
| separately launched exe at another path not owned | **BLOCKED: requires Windows CI** | Same mechanism as above |

### Startup failure cleanup (2 rows) — both PASS (fixed mid-session)

| Criterion | Status | Evidence |
|---|---|---|
| immediate failure removes state, returns 1, points to `logs` | **PASS** (was FAIL) | See "Headline finding." Found FAILing (message silently discarded by the BUG-1 stderr redirection); fixed mid-session; new suite `test_immediate_startup_failure_cleans_up` now passes and a direct ad hoc repro confirms the message is delivered |
| no sensitive log lines echoed into the failure message | **PASS** (was FAIL) | Same test/fix. Now genuinely verified: the delivered message is the fixed, non-log-content string, not merely an absence of any message |

### Default / overridden startup wait (3 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| default 1000 ms / env override shortens | **PASS** | New suite: `test_default_startup_wait_is_about_1000ms` (measured ≥900ms with no override); every other test uses `BUZZTALK_GATEWAY_STARTUP_WAIT_MS=10` successfully |
| invalid override rejected as config error | **PASS** | `tests/gateway.sh: test_startup_wait_override_is_validated` (value `0` rejected) |
| env var only affects survival check, not gateway timeouts | **PASS** | Source: `STARTUP_WAIT_MS`/`$script:StartupWaitMs` are used only in the post-launch `sleep`/`Start-Sleep` call, never added to the `buzztalkd` argument list |

### VPIO platform handling (4 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| macOS includes `--vpio` when enabled; Windows rejects it | **PASS** | `tests/gateway.sh: test_macos_launcher_includes_vpio_when_enabled`; Windows rejection at `Read-Configuration` (buzztalk-gateway.ps1:74-76) source-reviewed, not executed |
| Windows `VPIO=true` yields config error, not silent drop | **PASS (source)** | Same lines — explicit `Fail 'VPIO=true is only supported on macOS...' 2`, not a silent flag omission |
| exact error text | **PASS (source)** | Literal string at buzztalk-gateway.ps1:75 matches the spec's required text exactly, char for char |
| `configure` defaults VPIO true macOS / false Windows | **PASS** | macOS `configure_gateway` defaults `vpio=true` unless the user answers `n` (scripts/buzztalk-gateway:256); Windows `Configure-Gateway` hardcodes `'VPIO=false'` unconditionally (buzztalk-gateway.ps1:234) |

### Secret-safe output (6 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| key bytes never in stdout/stderr/state/helper logs | **PASS** | `tests/gateway.sh` (stderr check) + new suite `test_no_secret_material_written_to_state_or_log_files` (filesystem scan of every file under `STATE_DIR` after a full on/status/off cycle) |
| status/error output excludes secrets and resolved paths | **PASS** | Same tests; note the binary-not-found case is affected by BUG-1 — the message is *absent* rather than *sanitized* (see Headline finding); the content of the coded string itself never includes a path |
| all message paths scanned for leak patterns | **PASS (spot-checked)** | Every message path actually exercised by the two suites was scanned for relay/channel/pubkey/secret substrings with none found; this is not an exhaustive fuzz of every code path |
| `on`/`off`/`toggle`/`status` never auto-echo a log tail | **PASS** | Source: none of these four functions reference `$LOG_FILE`/`$StdoutLog`/`$StderrLog`; only `logs` does. Confirmed behaviorally — none of the dozens of on/off/status/toggle invocations across both suites ever printed log content |
| `status` omits the resolved log path | **PASS** | Source: `status_gateway`/`Get-GatewayStatus` never reference the log path variables |
| binary-not-found error omits resolved path | **PASS (content)** | The message string itself is static and contains no path interpolation — true regardless of BUG-1's delivery failure |

### Installer / release rollback (4 rows)

| Criterion | Status | Evidence |
|---|---|---|
| failed helper stage triggers full rollback | **PASS (macOS)** | `tests/installers.sh: test_second_replacement_failure_rolls_back_first_binary`, `test_signal_after_first_replacement_restores_both_binaries`. Note: the injected failure lands on the 2nd binary in the loop (`buzztalk-demo`), not specifically `buzztalk-gateway` — but the rollback code has no per-binary special-casing, so this is strong structural evidence, not a helper-targeted failure injection |
| correct helper packaged/staged/installed/rolled back | **PASS (macOS)** | `tests/installers.sh: test_macos_installs_matching_gateway_helper`; Windows equivalent (`tests/installers.ps1`) exists, not executed |
| macOS installer marks helper executable | **PASS** | `install.sh:196` `chmod +x "$payload/$binary"` applies uniformly to `install_files` (includes `buzztalk-gateway` on macOS targets); test asserts `[ -x "$install_dir/buzztalk-gateway" ]` |
| Windows installer places script beside `buzztalkd.exe` | **BLOCKED: requires Windows CI** | `install.ps1`'s `$destinations` hash includes `buzztalk-gateway.ps1` alongside the two `.exe` files with the identical backup/replace/rollback transaction (source-reviewed); not executed |

### README command smoke checks (3 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| README examples match help output | **PASS** | New suite: `test_readme_six_commands_are_all_recognized` — cross-checks `buzztalk-gateway help` output and README.md text for all six command names |
| bypass example runs, notes scoped-only | **PASS** | README.md:151-153 text explicitly states scope; the documented `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ...` syntax is visually correct standard PowerShell invocation syntax (cannot be executed — no pwsh) |
| all six commands present and smoke-runnable | **PASS** | Every one of the six (`configure`, `on`, `off`, `toggle`, `status`, `logs`) was actually invoked at least once across the suites plus the ad hoc `logs` runs above |

### Remaining shared behavioral cases (7 rows) — all PASS

| TS# | Status | Evidence |
|---|---|---|
| #1 missing config exits 2 with configure hint | **PASS** | `tests/gateway.sh: test_missing_config_is_actionable` |
| #2 invalid/duplicate keys rejected | **PASS** | New suite: `test_config_rejects_unknown_and_duplicate_keys` (unknown key + duplicate `RELAY` line, both via `on`) |
| #5 `on` records a live fake process, exits 0 | **PASS** | `tests/gateway.sh` tests 2–4 |
| #7 `status` exit 0 running / exit 3 stopped | **PASS** | `tests/gateway.sh: test_lifecycle_is_idempotent_and_status_exit_codes_are_stable` |
| #8 `toggle` cycles stopped→running→stopped | **PASS** | New suite: `test_toggle_cycles_stopped_running_stopped` |
| #9 `off` terminates only the recorded owned process | **PASS** | New suite ownership-mismatch test proves a non-matching identity is never signaled; lifecycle test proves the matching one is |
| #19 paths/logs honor env overrides in tests | **PASS** | Every test in every suite relies on `BUZZTALK_GATEWAY_CONFIG_DIR`/`_STATE_DIR` actually redirecting the launcher — dozens of executions, all isolated to `mktemp -d` fixtures |

### Non-interactive / overwrite-confirmation (2 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| refuses overwrite without confirmation | **PASS** | New suite: `test_configure_refuses_overwrite_without_confirmation`, using a real pty via Python's `pty.fork()` (a plain pipe does **not** exercise this path — see note below) to answer "n" interactively; config file byte-for-byte unchanged afterward |
| non-interactive + missing config never prompts, exits w/ config error | **PASS** | New suite: `test_configure_noninteractive_missing_config_no_prompt` (stdin from `/dev/null`, exit 2, no config file created, no hang). **Note for the implementer:** the exact message on this path is `Configuration not found. Run configure interactively.`, which differs from the message text used everywhere else in the script for "configuration missing" (`Configuration not found. Run: buzztalk-gateway configure`) — worth reconciling, though not itself an acceptance-criteria violation since the spec only requires exit code + "prints the required configure command," which this arguably still does in substance |

### Stop transaction sequencing (3 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| TERM, poll 5s, force KILL only after timeout | **PASS** | Ad hoc: fake binary that traps and ignores `TERM`; `off` took ~5s, then the process was confirmed gone (KILL fallback fired) |
| PID record removed only after confirmed absent | **PASS** | Same run — state file was present throughout the 5s poll and removed only after the process was confirmed dead |
| macOS TERM→KILL / Windows Stop-Process→-Force | **PASS (macOS)** | Same run for macOS; Windows `Stop-Gateway` (buzztalk-gateway.ps1:201-215) implements the identical `Stop-Process` → poll 5s → `Stop-Process -Force` shape (source-reviewed, not executed) |

## Section 4 — Exit-code contract (4 rows) — all PASS

| Criterion | Status | Evidence |
|---|---|---|
| `0` success / status running | **PASS** | Every successful `on`/`off`/`configure` and running `status` across all suites |
| `1` startup/shutdown/ownership failure | **PASS** | Ownership-mismatch test, immediate-startup-failure test, 5s-TERM/KILL test — exit code correct even where BUG-1 hides the message |
| `2` invalid command or config | **PASS** | New suite: `test_invalid_command_exits_2`; `tests/gateway.sh` missing-config and invalid-startup-wait tests; new suite unknown/duplicate-key test |
| `3` status stopped | **PASS** | `tests/gateway.sh: test_lifecycle_is_idempotent_and_status_exit_codes_are_stable` |

---

## Totals (final, current working-tree state)

| Status | Count |
|---|---|
| PASS | 72 |
| FAIL | 0 |
| GAP | 0 |
| BLOCKED (requires Windows CI) | 12 |
| **Total rows** | **84** |

**One bug was found and fixed during this session** (not by this audit — by
the other agent working the same tree, in response to the same defect this
audit's new tests surfaced). It briefly caused 3 rows to FAIL (AC8 and both
Startup-failure-cleanup rows in Section 3, all one root cause: "BUG-1", the
bare-`exec 2>/dev/null` stderr-swallowing bug in `validate_key_file()`).
All three are PASS as of the final re-run reflected in this document. See
"Headline finding" for the full history, repro, and fix.

No row was left as a genuine GAP: every scenario the ToDo called out by name
(key-file missing/unreadable, literal argument handling, duplicate launch
prevention, stale/reused PID safety, Windows ownership, startup-failure
cleanup, startup wait, VPIO, secret-safe output, installer rollback, README
smoke) now has either an executed test or documented source-review evidence
and a BLOCKED reason where execution genuinely requires Windows.

---

## New file created

`~/Code/buzztalk/tests/gateway-acceptance-extra.sh` — 12 new hardware-free
tests. Found the BUG-1 defect on first run (10 passed, 2 correctly failed);
now 12/12 pass against the current, patched launcher. Does not modify any
existing file. Run with `sh tests/gateway-acceptance-extra.sh`.

## Secret/path leak check on launcher output itself

None found. Every stdout/stderr capture across every suite and every ad hoc
run in this session was grepped for the fixture's relay URL, channel id,
agent pubkey, and the literal secret string `secret-that-must-not-be-read`
(or `secret` in the ad hoc runs) — zero matches outside of the fixture
config files themselves. The one place a message goes missing entirely
(BUG-1) is a visibility bug, not a leak.

## Results file

`/private/tmp/claude-501/-Users-michaelrobinson-Code-mrtek-product-portfolio/bfd956bb-5ae9-4b2f-8e0c-8b2a44a83dac/scratchpad/todo53-acceptance-audit-RESULTS.md`
(this file)
