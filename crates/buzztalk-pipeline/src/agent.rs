//! The [`AgentBackend`] trait: how the pipeline talks to whatever produces
//! the agent's replies, plus [`EchoAgent`], a local, network-free backend
//! for demonstrating and testing the full loop without Buzz.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use buzztalk_session::TurnId;

/// One event an [`AgentBackend`] can produce in response to a submitted
/// turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A chunk of the agent's reply text arrived. Chunks accumulate in
    /// order; there is no guarantee about how much text each one holds.
    TextChunk {
        /// The turn this chunk belongs to.
        turn: TurnId,
        /// The chunk of text.
        text: String,
    },
    /// The agent has no more text coming for this turn.
    TurnComplete {
        /// The turn that just finished producing text.
        turn: TurnId,
    },
}

/// Whatever produces the agent's replies.
///
/// `buzztalk-pipeline` depends on this trait, not on Buzz itself, so the
/// conversation loop can be exercised -- and tested -- without a live agent
/// connection. Both methods are called from the orchestration loop and must
/// not block: `submit` hands off and returns immediately, `poll` is a
/// non-blocking check for the next available event.
pub trait AgentBackend: Send {
    /// Hand the backend a finalized user utterance for `turn`. Returns
    /// immediately; the reply (if any) arrives later through [`Self::poll`].
    fn submit(&mut self, turn: TurnId, text: &str);

    /// Non-blocking poll for the next available event. `None` if nothing is
    /// ready yet.
    fn poll(&mut self) -> Option<AgentEvent>;
}

/// Canned replies [`EchoAgent`] cycles through. The third is deliberately
/// long (five-plus sentences) so a live demo has something substantial to
/// interrupt mid-reply.
const CANNED_REPLIES: &[&str] = &[
    "Sure, I can help with that. Let me take a look and get right back to you.",
    "Got it, that's on the way. Give me just a moment to sort it out.",
    "Sure, happy to walk through that in detail. First, the short version: \
     yes, this is possible, and it is safer than it sounds once the pieces \
     are in place. The main idea is to keep capture and playback on one \
     clock, so the echo canceller always has a true reference signal to \
     subtract. Once that is true, the barge-in detector can trust what it \
     hears instead of guessing. From there, endpointing and interruption \
     become two ordinary voice-activity detectors tuned for opposite \
     mistakes, running over the same cleaned audio. Put those pieces \
     together and the result is a conversation you can actually interrupt, \
     which is the whole point.",
];

/// A local, deterministic [`AgentBackend`] that replies with a canned
/// multi-sentence answer, delivered a phrase at a time with a small delay --
/// so the full capture -> STT -> agent -> TTS -> playback loop can be
/// demonstrated and tested with no Buzz connection and no network.
///
/// Replies cycle through [`CANNED_REPLIES`] on every [`AgentBackend::submit`]
/// call, regardless of what was said -- this is a test double, not a
/// language model.
pub struct EchoAgent {
    next_reply: usize,
    chunk_delay: Duration,
    events_rx: Receiver<AgentEvent>,
    events_tx: Sender<AgentEvent>,
}

impl EchoAgent {
    /// A fresh agent with the default chunk delay (180 ms) -- long enough
    /// that a live demo has time to interrupt a reply mid-sentence, short
    /// enough that a full canned reply lands well within a few seconds.
    pub fn new() -> Self {
        Self::with_chunk_delay(Duration::from_millis(180))
    }

    /// A fresh agent whose text chunks are delivered `chunk_delay` apart.
    pub fn with_chunk_delay(chunk_delay: Duration) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            next_reply: 0,
            chunk_delay,
            events_rx,
            events_tx,
        }
    }
}

impl Default for EchoAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentBackend for EchoAgent {
    fn submit(&mut self, turn: TurnId, _text: &str) {
        let reply = CANNED_REPLIES[self.next_reply % CANNED_REPLIES.len()];
        self.next_reply += 1;

        let chunks = buzztalk_tts::segment_into_phrases(reply);
        let tx = self.events_tx.clone();
        let delay = self.chunk_delay;
        thread::spawn(move || {
            for chunk in chunks {
                thread::sleep(delay);
                if tx
                    .send(AgentEvent::TextChunk { turn, text: chunk })
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(AgentEvent::TurnComplete { turn });
        });
    }

    fn poll(&mut self) -> Option<AgentEvent> {
        self.events_rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzztalk_session::{Input, SessionMachine};
    use std::time::Instant;

    /// Mint a real `TurnId` the only way one is obtainable outside
    /// `buzztalk-session`: drive an actual `SessionMachine`.
    fn a_turn() -> TurnId {
        let mut machine = SessionMachine::new();
        machine.handle(Input::SessionStart);
        machine.handle(Input::PushToTalkPressed);
        machine.current_turn().expect("push-to-talk starts a turn")
    }

    #[test]
    fn poll_is_none_before_any_submission() {
        let mut agent = EchoAgent::new();
        assert_eq!(agent.poll(), None);
    }

    #[test]
    fn submit_eventually_delivers_text_chunks_tagged_with_the_submitted_turn() {
        let mut agent = EchoAgent::with_chunk_delay(Duration::from_millis(1));
        let turn = a_turn();
        agent.submit(turn, "hello");

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_chunk = false;
        while Instant::now() < deadline {
            match agent.poll() {
                Some(AgentEvent::TextChunk { turn: t, text }) => {
                    assert_eq!(t, turn, "every chunk must carry the submitted turn");
                    assert!(!text.is_empty());
                    saw_chunk = true;
                }
                Some(AgentEvent::TurnComplete { turn: t }) => {
                    assert_eq!(t, turn);
                    break;
                }
                None => thread::sleep(Duration::from_millis(2)),
            }
        }
        assert!(
            saw_chunk,
            "expected at least one TextChunk before completion"
        );
    }

    #[test]
    fn a_reply_eventually_completes_with_turn_complete() {
        let mut agent = EchoAgent::with_chunk_delay(Duration::from_millis(1));
        let turn = a_turn();
        agent.submit(turn, "hello");

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut completed = false;
        while Instant::now() < deadline {
            if let Some(AgentEvent::TurnComplete { turn: t }) = agent.poll() {
                assert_eq!(t, turn);
                completed = true;
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(completed, "expected TurnComplete within the deadline");
    }

    #[test]
    fn one_canned_reply_is_long_enough_to_interrupt() {
        // Requirement: at least one canned reply must segment into 5+
        // sentence-sized phrases, so a live demo has something substantial
        // to barge in on mid-reply.
        let longest = CANNED_REPLIES
            .iter()
            .map(|r| buzztalk_tts::segment_into_phrases(r).len())
            .max()
            .unwrap_or(0);
        assert!(
            longest >= 5,
            "expected a canned reply segmenting into >=5 phrases, longest was {longest}"
        );
    }

    #[test]
    fn replies_cycle_across_submissions() {
        let mut agent = EchoAgent::with_chunk_delay(Duration::from_secs(60));
        let turn = a_turn();
        // Submitting N+1 times should wrap back to reply 0's first chunk.
        for _ in 0..CANNED_REPLIES.len() {
            agent.submit(turn, "x");
        }
        assert_eq!(agent.next_reply, CANNED_REPLIES.len());
    }
}
