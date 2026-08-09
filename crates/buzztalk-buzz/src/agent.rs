//! [`BuzzAgent`]: the [`AgentBackend`] that talks to a real Buzz relay.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use buzztalk_pipeline::{AgentBackend, AgentEvent};
use buzztalk_session::TurnId;

use crate::config::BuzzConfig;
use crate::error::BuzzError;
use crate::transport;

/// Commands the foreground [`BuzzAgent`] hands to its background transport
/// thread. Sending never blocks (`std::sync::mpsc::Sender::send` on an
/// unbounded channel never blocks the sender) -- that, plus doing no other
/// work, is what lets [`AgentBackend::submit`] return immediately.
pub(crate) enum Command {
    /// Publish this text as a fresh turn submission.
    Publish(String),
    /// Stop the transport thread and close the connection.
    Shutdown,
}

/// Speaks to a real Buzz relay on behalf of `buzztalk-pipeline`.
///
/// # Turn attribution
///
/// Buzz's wire protocol has no notion of BuzzTalk's [`TurnId`]: a kind:9
/// reply from an agent pubkey carries no correlation back to which
/// submission it answers. `BuzzAgent` resolves this the only way it can
/// without protocol support -- **the most recently submitted turn is
/// always the addressee of the next inbound reply**:
///
/// * every [`AgentBackend::submit`] call makes its `turn` the *current*
///   turn, superseding whatever was current before, and resets the
///   quiet-period timer;
/// * every inbound reply the transport thread judges speakable (see
///   [`crate::eligibility`]) is attributed to whatever turn is current at
///   the moment it arrives, or dropped if no turn has ever been submitted;
/// * **the ambiguity**: if two turns are ever in flight at once
///   (submission B is sent before Buzz has finished replying to submission
///   A), any reply to A that arrives *after* B is submitted is
///   misattributed to B. Nothing client-side can fix this correctly --
///   Buzz doesn't thread a reply back to the request that prompted it, so
///   there is no signal left to disambiguate with. `buzztalk-session`'s
///   single-turn-at-a-time model makes this rare in practice (a new turn
///   isn't submitted until the previous one has ended, via
///   [`AgentEvent::TurnComplete`], a barge-in, or a timeout), but it is not
///   impossible, and this crate does not pretend otherwise.
///
/// # Turn completion is a timeout, not a signal
///
/// Buzz posts each agent reply as one or more complete kind:9 messages, not
/// a token stream -- there is no explicit "done" event on the wire.
/// `BuzzAgent` therefore treats [`AgentEvent::TurnComplete`] as "no further
/// reply text arrived within the configured quiet period", exactly like an
/// inactivity timeout, not a protocol guarantee. Two consequences:
///
/// * a reply that takes longer than the quiet period to post can trigger a
///   premature `TurnComplete` while more text is still coming;
/// * an agent that posts its reply as several separate kind:9 messages
///   produces its own `TextChunk`/`TurnComplete` pair per message, not one
///   pair for the whole reply -- `buzztalk_pipeline`'s turn-currency checks
///   make extra `TurnComplete`s for an already-finished turn harmless (they
///   are simply dropped as stale), so this is a labeling quirk, not a
///   correctness bug, but it is a real limitation of not having a
///   streaming protocol underneath.
pub struct BuzzAgent {
    cmd_tx: Sender<Command>,
    reply_rx: Receiver<String>,
    quiet_period: Duration,
    current_turn: Option<TurnId>,
    last_reply_at: Option<Instant>,
    worker: Option<JoinHandle<()>>,
}

impl BuzzAgent {
    /// Load the configured key, spawn the background transport thread, and
    /// return immediately -- the actual connection (and NIP-42
    /// authentication, and any retries) happen asynchronously on that
    /// thread. A relay that is unreachable at startup does not fail this
    /// call; it keeps retrying with backoff (see [`crate::transport`]).
    ///
    /// The only synchronous failure mode is the [`crate::KeySource`] itself
    /// being unusable (env var unset, file missing, key malformed) --
    /// there is no point starting a thread that can never authenticate.
    pub fn connect(config: BuzzConfig) -> Result<Self, BuzzError> {
        let keys = config.key_source.load()?;
        let quiet_period = config.reply_quiet_period;
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();

        let worker = thread::Builder::new()
            .name("buzztalk-buzz-transport".into())
            .spawn(move || transport::run(config, keys, cmd_rx, reply_tx))
            .map_err(|e| BuzzError::Spawn(e.to_string()))?;

        Ok(Self::from_parts(
            cmd_tx,
            reply_rx,
            quiet_period,
            Some(worker),
        ))
    }

    /// Assemble a [`BuzzAgent`] from its channel halves without spawning a
    /// real transport thread. Used by `BuzzAgent::connect` (with a real
    /// worker) and directly by this module's tests (with a fake transport:
    /// the test itself plays both ends of `cmd_rx`/`reply_tx`, no network
    /// or thread involved) -- see the task brief's requirement that turn
    /// mapping and the `AgentBackend` contract be testable without one.
    fn from_parts(
        cmd_tx: Sender<Command>,
        reply_rx: Receiver<String>,
        quiet_period: Duration,
        worker: Option<JoinHandle<()>>,
    ) -> Self {
        Self {
            cmd_tx,
            reply_rx,
            quiet_period,
            current_turn: None,
            last_reply_at: None,
            worker,
        }
    }
}

impl AgentBackend for BuzzAgent {
    fn submit(&mut self, turn: TurnId, text: &str) {
        self.current_turn = Some(turn);
        // A fresh turn has no reply yet, so no completion timer should be
        // running for whatever the previous turn's last reply happened to
        // set.
        self.last_reply_at = None;
        // `Sender::send` on this unbounded channel never blocks; its only
        // failure is the receiver having hung up (the transport thread
        // exited), which this non-fallible trait method has no way to
        // report back through -- drop it. The transport thread logs its
        // own exit to stderr.
        let _ = self.cmd_tx.send(Command::Publish(text.to_string()));
    }

    fn poll(&mut self) -> Option<AgentEvent> {
        loop {
            match self.reply_rx.try_recv() {
                Ok(text) => {
                    let Some(turn) = self.current_turn else {
                        // A reply arrived with no live submission to
                        // attribute it to (e.g. stray channel traffic
                        // before this agent ever submitted anything).
                        // Nothing sensible to attach it to -- drop and
                        // keep draining in case a real one follows.
                        continue;
                    };
                    self.last_reply_at = Some(Instant::now());
                    return Some(AgentEvent::TextChunk { turn, text });
                }
                Err(TryRecvError::Empty) => break,
                // The transport thread exited (relay session ended for
                // good, or panicked). Nothing more will ever arrive on
                // this channel; fall through to the quiet-period check
                // below so whatever turn was in flight still completes
                // instead of hanging forever.
                Err(TryRecvError::Disconnected) => break,
            }
        }

        if let (Some(turn), Some(last)) = (self.current_turn, self.last_reply_at) {
            if last.elapsed() >= self.quiet_period {
                self.last_reply_at = None;
                return Some(AgentEvent::TurnComplete { turn });
            }
        }

        None
    }
}

impl Drop for BuzzAgent {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_session::{Input, SessionMachine};

    /// Build a `BuzzAgent` wired to a fake transport: the test owns both
    /// channel halves directly (`cmd_rx` to observe what `submit` sends,
    /// `reply_tx` to inject inbound replies) with no thread, no socket, no
    /// network at all.
    fn fake() -> (BuzzAgent, Receiver<Command>, Sender<String>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let agent = BuzzAgent::from_parts(cmd_tx, reply_rx, Duration::from_millis(30), None);
        (agent, cmd_rx, reply_tx)
    }

    /// Mint a real `TurnId` the only way one is obtainable outside
    /// `buzztalk-session`: drive an actual `SessionMachine`. Mirrors
    /// `buzztalk-pipeline`'s own `EchoAgent` tests.
    fn a_turn() -> TurnId {
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        machine.handle(Input::PushToTalkPressed);
        machine.current_turn().expect("push-to-talk starts a turn")
    }

    fn another_turn() -> TurnId {
        // A second, independently driven machine mints a `TurnId` that
        // compares unequal to `a_turn()`'s -- both start counting from 0
        // internally, so drive this one through two turns and keep the
        // second, which `SessionMachine`'s monotonically-increasing
        // counter guarantees differs from a fresh machine's first turn
        // only by construction, not by chance.
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        machine.handle(Input::PushToTalkPressed);
        let first = machine.current_turn().expect("first turn");
        machine.handle(Input::PushToTalkReleased);
        machine.handle(Input::FinalTranscript {
            turn: first,
            text: "hi".to_string(),
        });
        machine.handle(Input::SubmitSucceeded { turn: first });
        machine.handle(Input::AgentTurnComplete { turn: first });
        machine.handle(Input::PushToTalkPressed);
        machine.current_turn().expect("second turn")
    }

    #[test]
    fn submit_never_blocks_and_forwards_text_as_a_publish_command() {
        let (mut agent, cmd_rx, _reply_tx) = fake();
        let turn = a_turn();
        agent.submit(turn, "hello buzz");

        match cmd_rx.try_recv().expect("a command was sent") {
            Command::Publish(text) => assert_eq!(text, "hello buzz"),
            Command::Shutdown => panic!("expected Publish, got Shutdown"),
        }
    }

    #[test]
    fn poll_is_none_before_any_submission_even_with_stray_channel_traffic() {
        let (mut agent, _cmd_rx, reply_tx) = fake();
        // Simulate stray traffic on the subscribed channel that arrived
        // before this agent ever submitted a turn.
        reply_tx.send("unsolicited".to_string()).unwrap();
        assert_eq!(agent.poll(), None);
    }

    #[test]
    fn a_reply_is_tagged_with_the_submitted_turn() {
        let (mut agent, _cmd_rx, reply_tx) = fake();
        let turn = a_turn();
        agent.submit(turn, "question");
        reply_tx.send("the answer".to_string()).unwrap();

        assert_eq!(
            agent.poll(),
            Some(AgentEvent::TextChunk {
                turn,
                text: "the answer".to_string()
            })
        );
    }

    #[test]
    fn turn_completes_after_the_quiet_period_with_no_further_replies() {
        let (mut agent, _cmd_rx, reply_tx) = fake();
        let turn = a_turn();
        agent.submit(turn, "question");
        reply_tx.send("partial answer".to_string()).unwrap();
        assert_eq!(
            agent.poll(),
            Some(AgentEvent::TextChunk {
                turn,
                text: "partial answer".to_string()
            })
        );

        // Not complete yet -- quiet period hasn't elapsed.
        assert_eq!(agent.poll(), None);

        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(agent.poll(), Some(AgentEvent::TurnComplete { turn }));
        // Only fires once per quiet gap.
        assert_eq!(agent.poll(), None);
    }

    #[test]
    fn a_second_burst_after_completion_reopens_the_same_turn() {
        // Documents the "one TextChunk/TurnComplete pair per Buzz message"
        // limitation described in `BuzzAgent`'s docs: two separate replies
        // to the same still-current turn produce two completions.
        let (mut agent, _cmd_rx, reply_tx) = fake();
        let turn = a_turn();
        agent.submit(turn, "question");

        reply_tx.send("first burst".to_string()).unwrap();
        agent.poll();
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(agent.poll(), Some(AgentEvent::TurnComplete { turn }));

        reply_tx.send("second burst".to_string()).unwrap();
        assert_eq!(
            agent.poll(),
            Some(AgentEvent::TextChunk {
                turn,
                text: "second burst".to_string()
            })
        );
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(agent.poll(), Some(AgentEvent::TurnComplete { turn }));
    }

    #[test]
    fn a_new_submission_supersedes_the_previous_turn_for_attribution() {
        // The documented ambiguity resolution: whichever turn was
        // submitted most recently is the one the next reply attaches to.
        let (mut agent, _cmd_rx, reply_tx) = fake();
        let turn_a = a_turn();
        let turn_b = another_turn();
        assert_ne!(turn_a, turn_b);

        agent.submit(turn_a, "first question");
        agent.submit(turn_b, "second question, before A answered");

        // A reply that (from Buzz's perspective) might have been meant for
        // A arrives after B was submitted -- it is attributed to B, the
        // now-current turn. This is exactly the documented ambiguity.
        reply_tx.send("some answer".to_string()).unwrap();
        assert_eq!(
            agent.poll(),
            Some(AgentEvent::TextChunk {
                turn: turn_b,
                text: "some answer".to_string()
            })
        );
    }

    #[test]
    fn submitting_again_resets_the_quiet_period_timer() {
        let (mut agent, _cmd_rx, reply_tx) = fake();
        let turn_a = a_turn();
        agent.submit(turn_a, "q1");
        reply_tx.send("partial".to_string()).unwrap();
        agent.poll(); // TextChunk for turn_a

        // Most of the quiet period elapses...
        std::thread::sleep(Duration::from_millis(20));
        let turn_b = another_turn();
        // ...then a new turn is submitted, which must reset the timer
        // rather than letting a stale one immediately fire.
        agent.submit(turn_b, "q2");
        assert_eq!(
            agent.poll(),
            None,
            "a fresh submission must not immediately report completion"
        );
    }

    #[test]
    fn poll_does_not_panic_after_the_transport_thread_disconnects() {
        let (mut agent, cmd_rx, reply_tx) = fake();
        let turn = a_turn();
        agent.submit(turn, "q");
        drop(reply_tx);
        drop(cmd_rx);
        // Must not panic even though both ends of the fake transport are gone.
        assert_eq!(agent.poll(), None);
    }
}
