# BuzzTalk — brainstorming prompt

A self-contained prompt for thinking hard about BuzzTalk's direction with a model
that has no prior context. It deliberately carries the unflattering state of the
project alongside the good numbers, because a brainstorm grounded in a flattering
summary produces flattering answers.

Paste everything below the line.

---

# BuzzTalk — help me work out what would make this genuinely good

I've built a working thing and I want to think hard about whether it's the right
thing, and what would make it exceptional rather than merely functional. I want
your judgement, not a feature list.

## What it is

BuzzTalk is a full-duplex voice conversation layer for Buzz (github.com/block/buzz),
an open-source team chat platform where AI agents and humans are both first-class
members. You talk, it transcribes locally, your speech becomes a normal chat
message, an agent replies, the reply is spoken aloud — and crucially you can
interrupt the agent mid-sentence by just talking, and it stops.

Buzz already had good local speech: NVIDIA Parakeet recognition, Kyutai Pocket
TTS, voice activity detection, push-to-talk, and a cancellation path that silences
playback in ~15ms. What it could not do was let you interrupt by speaking. The
reason was architectural: Buzz captures the microphone in its webview and plays
synthesized speech from its Rust process, so the echo canceller has no reference
signal for the app's own voice. Buzz correctly responds by muting the mic entirely
while an agent talks. BuzzTalk owns the audio device so capture and playback share
one clock, which makes the exact output samples available as that reference.

## What actually exists and is measured

10 Rust crates, 257 tests, Apache-2.0, shipping binaries for macOS/Linux/Windows.

- barge-in to silence: 7–15 ms
- end of speech to final transcript: 72–203 ms
- echo cancellation: 36.6 dB (synthetic)
- speech synthesis: ~4x faster than real time
- 481 µs of a 10 ms real-time budget per frame
- proven end to end against a real Buzz relay: authenticated, published a signed
  message, read it back with Buzz's own CLI

## What is NOT true yet, stated plainly

- **No AI agent has ever replied to it.** It publishes speech and subscribes for
  replies; nothing has answered. Turn attribution, "the agent finished speaking"
  detection, and barge-in against real agent audio are all untested.
- **Every echo-cancellation number is synthetic.** No microphone has heard a real
  speaker. Barge-in over loudspeakers is unverified.
- **There is no UI.** It's a terminal program. No mic button, no live transcript
  in the composer.
- Agent replies arrive as complete messages rather than streaming, so "start
  speaking before the answer finishes" mostly doesn't happen yet.

## The strategic fork I need to decide

BuzzTalk drifted from its own design. The plan was to *reuse* Buzz's speech
engines. In practice I built parallel crates, and the synthesis crate is a port of
Buzz's own engine. There are now two implementations that will drift apart.

1. Upstream the whole conversation layer into Buzz, delete the duplication
2. Stay standalone and accept duplication as the price of moving fast
3. Invert it — BuzzTalk becomes the engine and Buzz depends on it

## What I want from you

Think about this as a product person with strong technical taste, not as an
engineer taking a ticket. Specifically:

1. **Challenge the premise.** Is "interrupt the AI mid-sentence" actually the thing
   that matters, or is it an impressive demo that people use twice? What would
   change your mind either way?

2. **What makes voice interfaces fail?** Not technically — socially and
   habitually. Most people try voice assistants and drift back to typing. What
   specifically causes that, and does anything here address it?

3. **What is the smallest thing that would make someone prefer this to typing?**
   For a real task, not a demo. I suspect it isn't latency.

4. **Name what to cut.** I have telephony, multi-agent voice scheduling, and
   several conversation modes designed but unbuilt. Argue for killing some.

5. **The multi-agent question.** Buzz makes AI agents visible channel members. A
   channel could have several agents speaking aloud. That could be remarkable or
   unbearable. Which, and what determines it?

6. **The strategic fork above** — pick one and defend it.

7. **What am I not seeing?** The thing that will be obvious in hindsight.

## How to answer

Be opinionated and concrete. Prefer one sharp argument over five hedged ones. If
you think a direction is wrong, say so directly and say why. Use examples and
scenarios rather than abstractions. Skip the executive summary, skip
"here are some considerations," and don't produce a bulleted feature list — I can
generate those myself, and they're what I'm trying to avoid.

If you need to know something about how it works to answer well, ask me rather
than assuming.
