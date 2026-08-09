# Voice-driven agent crew — setup and demo

*Reproducible from the 2026-08-09 live session. This is the "speak, and a
team of agents divides the work" demo — the one that shows why voice on
Buzz is more than talking to a single bot.*

## What it demonstrates

One person speaks into a headset. Their words appear in a Buzz channel
under their own name, a **crew of AI agents** reads them, delegates work
among themselves, and a single narrator voice reads the result back — all
without touching a keyboard. Verified end to end on a real hosted Buzz
community with Opus-backed agents:

```
Michael (voice) | Beyond the Leader is an excellent successor to the Follower Effect book.
Coordinator     | @Researcher verify the real titles and authors, two lines on what each argues.
Michael (voice) | ...have Scribe log my take as well.
Coordinator     | @Scribe log Michael's take; mark titles voice-transcribed, pending Researcher.
Coordinator     | Both real, both by Tony Bridwell, Wiley business fables. Follower Effect
                  says followers set the outcome; Beyond the Leader lays out seven
                  disciplines for teams that keep learning.   (← spoken back)
Coordinator     | Scribe logged it. Nuance: Wiley doesn't bill it as a sequel, so the
                  "successor" call is yours.                   (← spoken back)
```

Coordinator delegated *autonomously* — the speaker never said "ask
Researcher"; the agent recognized the task and fanned it out, then
distinguished the speaker's opinion from the verified record.

## The key ideas

- **Voice input is signed as the human.** `buzztalkd` runs with the user's
  own signing key, so spoken words publish as ordinary `kind:9` messages
  from the user's identity — indistinguishable from typing, attributed by
  key, not by any voice-specific mechanism.
- **Agents are reached by p-tag mention.** Buzz agents run with
  `subscribe=Mentions`, so they only *see* messages that p-tag them.
  `buzztalkd --agent-pubkey <pk>` p-tags every spoken message to that
  agent automatically — which is also how the daemon decides whose replies
  to speak.
- **Dispatcher, not conference call.** Only the Coordinator is on the
  spoken (`--agent-pubkey`) list. Researcher and Scribe post to the
  channel (visible in the app) but are *not* read aloud. The human hears
  one narrator summarizing a working crew, not three agents at once.
- **Owner-only agents answer their owner without a mention prompt.** Buzz
  agents created by the user default to `respond_to=owner-only`, so once
  p-tagged they respond to the owner's turn directly.

## Setup

### 1. The agents (in Buzz Desktop, in the user's real community)

Create three agents through the app's Add-Agent flow (this registers them
as real Buzz agents — `kind:30003` owner-binding — so they appear in the
roster and the app runs their runtimes). Roles and instructions:

- **Coordinator** — the dispatcher and the only spoken voice. Delegates to
  teammates by @mention, then gives the human a one-or-two-sentence spoken
  summary. Never reads long content aloud. (System prompt:
  `references/coordinator.prompt` below.)
- **Researcher** — answers facts / lookups when @mentioned by Coordinator.
- **Scribe** — records notes, action items, decisions when @mentioned.

A registered agent's runtime only picks up a channel once it's a member,
via a membership notification — so add all three to the channel *after*
creating them (or restart the app so it re-discovers). A newly created
agent that shows `discovered 0 channel(s) — will sit idle` just hasn't
been added to a channel yet.

### 2. The channel

Create a channel (we used **The Bridge**) in the same community and add
Coordinator, Researcher, and Scribe. The user is owner/admin.

### 3. The voice daemon

```
buzztalkd \
  --relay wss://<your-community>.communities.buzz.xyz \
  --channel <channel-uuid> \
  --agent-pubkey <coordinator-pubkey> \   # p-tagged + spoken; Coordinator only
  --key-file <path-to-user-nsec> \        # signs spoken messages as the user
  --vpio \                                # full-duplex on Bluetooth (macOS)
  --headphones \                          # in-ear route (no loudspeaker echo path)
  --endpoint-silence-ms 700               # pause tolerance for thinking-aloud speech
```

`--relay` accepts `wss://` (TLS via rustls). The key file holds the user's
`nsec1…` (or hex); it is read once and never logged. The channel and the
agents must be on the **same relay** — the most common failure in the live
session was the agents living on the hosted community while a test channel
sat on a local dev relay, so they never shared a room.

## Gotchas hit live (all now handled)

- **`subscribe=Mentions`**: plain text saying "Coordinator, ..." is not a
  mention. Must p-tag the agent. `buzztalkd` does this; the manual CLI
  smoke test had to add `--mention <pk>`.
- **Agent sits idle**: it wasn't a channel member yet, or its runtime
  connected to a different relay than the channel. Check the harness log's
  `discovered N channel(s)` and `relay=` lines.
- **Permission denials**: an app-run agent has its CLI configured for it;
  a hand-run `buzz-acp` needs a `.claude/settings.local.json` allowlisting
  `Bash(buzz messages send:*)` or the agent replies into the void under
  `dontAsk` mode.
- **Bluetooth mic degrades mid-session**: handled by the dead-capture
  watchdog (three wordless speech turns → automatic engine rebuild) plus
  the device-signature watchdog — the daemon self-heals rather than going
  permanently deaf.

## references/coordinator.prompt

> You are Coordinator, the dispatcher in this channel. A human named
> Michael talks to you by voice; your replies are spoken aloud by
> text-to-speech, so keep them to one or two short conversational
> sentences, no markdown, no lists. Delegate rather than answer everything
> yourself. Two teammates are here: @Researcher (facts, lookups,
> explanations) and @Scribe (notes, action items, decisions). When Michael
> asks for something, delegate by @mentioning the right teammate with a
> clear instruction; you may delegate to both. Once they reply, give
> Michael a brief spoken summary. Small talk you can answer yourself,
> briefly. Never read long content aloud — summarize.

*(Researcher and Scribe prompts are the obvious one-paragraph analogues —
stay in lane, answer/record when mentioned, keep replies tight because
Coordinator reads them, not the human.)*

## A refinement worth making

Because the spoken list is per-agent, the human hears *everything*
Coordinator posts — including its `@Researcher ...` delegation messages,
not just the final summary. For a cleaner demo, instruct Coordinator to
keep delegation messages terse, or split "thinking aloud to the team" from
"summary for Michael" by a convention the daemon could learn to filter.
Not yet built.
