# BuzzTalk Launcher and Desktop Bridge Boundaries

**Date:** 2026-08-10

**Status:** Accepted, with dependency ownership unresolved

## Decision

The Gateway Launcher and Desktop Audio Bridge are complementary, independent capabilities:

- The **Gateway Launcher** manages the standalone `buzztalkd` process for CLI and operator use. It starts, stops, inspects, and tails that installed binary.
- The **Desktop Audio Bridge** embeds only the `buzztalk-audio` VoiceProcessingIO engine inside Buzz Desktop. Buzz continues to own Huddle identity, signing, relay, STT, TTS, models, agent routing, and message publication.

Neither capability is a stepping-stone, fallback product, or temporary substitute for the other. The launcher never controls Buzz Desktop. The bridge never launches `buzztalkd`, a sidecar, or a second relay or identity path.

## Dependency Ownership Gate

The launcher's contract ends at the installed `buzztalkd` executable and is independent of Rust crate publication or source ownership.

The bridge cannot merge while `buzztalk-audio` depends on an unresolved personal Git home. Before implementation begins, the bridge owner must document a recommendation among:

1. Publish `buzztalk-audio` to crates.io under a maintained owner and release policy.
2. Vendor the minimal audio crate into the Buzz monorepo under explicit Buzz ownership.
3. Use another maintained home accepted by Buzz maintainers, with named ownership, versioning, update, security-response, and license-provenance rules.

A personal Git revision may support a draft experiment only. The upstream PR remains draft and must not merge until maintainers accept an ownership model and the dependency manifest uses that maintained source.

## Platform Posture

- Launcher process management supports macOS and Windows. This is control-plane support and does not claim equivalent audio quality or hardware validation.
- The Desktop Audio Bridge is supported only on Apple Silicon macOS and exposes its control only when runtime capability is present. Intel macOS, Windows, and Linux render no disabled control and reserve no empty layout slot.
- Windows and Linux continue using Buzz's existing Huddle audio path. Neither document promises a future native bridge or Linux launcher package.

These platform decisions are independent: Windows launcher support neither enables nor implies a Windows Desktop Audio Bridge.

## Messaging and Logs

- Standalone `buzztalkd` logs are unbounded and may contain transcripts. Operators own rotation and cleanup.
- Launcher-generated status and error messages do not echo log tails and never include keys, key contents, transcripts, channel identifiers, relay URLs, or resolved executable paths. They provide a stable outcome and the next launcher command.
- Bridge telemetry contains stable phases, durations, and error codes only. User-visible errors add fixed recovery copy such as Retry or confirmation that standard Huddle audio was restored. Neither channel contains audio, keys, transcripts, channel identifiers, relay URLs, pubkeys, device paths, or raw OS/dependency errors.
- Native-start failure reports that standard Huddle audio was restored and offers Retry. Ownership mismatch fails closed and directs the operator to `status` or documented state recovery without signaling the process.

## Validation Boundary

- Launcher lifecycle and Windows full-path-plus-creation-time ownership checks use fake processes in CI and never require audio hardware.
- Desktop bridge CI uses fake duplex drivers and mocked Tauri transitions. Physical validation remains required, non-delegated, and limited to the Apple Silicon routes listed in the bridge plan.
- Hardware observations do not create support claims for unvalidated platforms.

## Decisions and Remaining Open Items

- **Accepted:** standalone launcher and in-process bridge are independent products with the hard boundaries above.
- **Accepted:** Windows launcher support is process-management only; the Desktop Audio Bridge is Apple Silicon macOS-only, capability-hidden elsewhere, and makes no future-platform promise.
- **Accepted:** launcher logs are transcript-bearing and operator-rotated; launcher-generated messages and bridge diagnostics use safe metadata and explicit next actions.
- **Accepted:** launcher CI is hardware-free; bridge hardware validation is required, non-delegated, and limited to the listed Apple Silicon routes.
- **Open — bridge dependency ownership:** obtain and record the Buzz maintainers' accepted home for `buzztalk-audio`, update the dependency manifest to that source, and attach the decision to the draft PR before requesting merge. No platform or messaging decision remains open in these two documents.
