//! The inbound "is this a speakable agent reply?" classifier.
//!
//! Every event on a subscribed channel passes through [`is_speakable`]
//! before [`crate::transport`] will forward it to [`crate::BuzzAgent`] as an
//! [`buzztalk_pipeline::AgentEvent::TextChunk`]. The rules below are Buzz's
//! own, as specified by this crate's task brief and cross-checked against
//! `NOSTR.md` (kind 9's `#h` requirement, kind 40002 as an equally valid
//! stream-message variant, self-authored/system-message conventions
//! visible elsewhere in the reference material): kind must be a stream
//! message, the `h` tag must match the subscribed channel, the author must
//! be a known agent pubkey and not this identity's own pubkey, and the
//! content must be non-empty and not start with Buzz's `"[System]"`
//! convention. Buzz's actual relay-side implementation of these checks
//! lives in the relay's source, which isn't part of the reference material
//! shipped to this crate -- so this is a from-scratch, table-tested
//! reimplementation of the rules as specified, not a port.

use nostr::event::{Event, Kind};
use nostr::key::PublicKey;
use uuid::Uuid;

use crate::kinds::{KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2};

/// Prefix Buzz's convention reserves for system messages -- never speakable.
pub const SYSTEM_PREFIX: &str = "[System]";

/// Why an event was judged not-speakable.
///
/// Never a hard error: rejecting most of a channel's traffic (reactions,
/// membership changes, human chatter, this agent's own messages) is the
/// expected, frequent outcome of subscribing to a whole channel, not a
/// failure condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Not a stream-message kind ([`KIND_STREAM_MESSAGE`] or
    /// [`KIND_STREAM_MESSAGE_V2`]).
    WrongKind,
    /// Missing an `h` tag, or it doesn't match the subscribed channel.
    ChannelMismatch,
    /// The event's author is this identity's own pubkey.
    SelfAuthored,
    /// The author is not in the configured agent pubkey list.
    UnknownAuthor,
    /// Content is empty, or all whitespace.
    Empty,
    /// Content starts with [`SYSTEM_PREFIX`].
    SystemMessage,
    /// The message p-tags other participants but not the user -- it's a
    /// dispatcher agent talking to its teammates (delegation), not
    /// answering the user, so it isn't read aloud. Only applies when
    /// [`EligibilityContext::speak_only_user_directed`] is set.
    NotAddressedToUser,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RejectReason::WrongKind => "not a stream-message kind",
            RejectReason::ChannelMismatch => "missing or mismatched h tag",
            RejectReason::SelfAuthored => "self-authored",
            RejectReason::UnknownAuthor => "author is not a known agent pubkey",
            RejectReason::Empty => "empty content",
            RejectReason::SystemMessage => "system message",
            RejectReason::NotAddressedToUser => "addressed to another agent, not the user",
        };
        f.write_str(s)
    }
}

/// Everything [`is_speakable`] needs to know about the subscription to
/// judge one event.
pub struct EligibilityContext<'a> {
    /// The channel BuzzTalk is bridging.
    pub channel_id: Uuid,
    /// This identity's own pubkey (never speaks its own messages back to
    /// itself). Because `buzztalkd` signs the user's spoken messages, this
    /// is also *the user's* pubkey -- a message that p-tags it is one
    /// addressed to the user.
    pub own_pubkey: PublicKey,
    /// Pubkeys BuzzTalk accepts replies from.
    pub agent_pubkeys: &'a [PublicKey],
    /// When set, an agent message that p-tags other participants but not
    /// the user ([`own_pubkey`]) is rejected as [`RejectReason::NotAddressedToUser`].
    /// This is what keeps a dispatcher agent's `@teammate ...` delegation
    /// chatter out of the spoken stream while still reading its "on it" and
    /// its relayed answers back to the user. A message with no p-tags at
    /// all is always eligible (a plain reply to the user).
    ///
    /// [`own_pubkey`]: Self::own_pubkey
    pub speak_only_user_directed: bool,
}

/// Decide whether `event` is a speakable agent reply, per Buzz's rules
/// (see the module docs).
///
/// Checks run in a fixed, most-certain-first order so a caller logging
/// [`RejectReason`] gets the most specific answer: an event's own kind and
/// channel are checked before anything about its author, and
/// self-authorship (this identity signed it) is checked before the more
/// general unknown-author case.
pub fn is_speakable(event: &Event, ctx: &EligibilityContext<'_>) -> Result<(), RejectReason> {
    if event.kind != Kind::Custom(KIND_STREAM_MESSAGE)
        && event.kind != Kind::Custom(KIND_STREAM_MESSAGE_V2)
    {
        return Err(RejectReason::WrongKind);
    }

    let channel_str = ctx.channel_id.to_string();
    let channel_matches = event
        .tags
        .iter()
        .any(|t| t.kind() == "h" && t.content() == Some(channel_str.as_str()));
    if !channel_matches {
        return Err(RejectReason::ChannelMismatch);
    }

    if event.pubkey == ctx.own_pubkey {
        return Err(RejectReason::SelfAuthored);
    }

    if !ctx.agent_pubkeys.contains(&event.pubkey) {
        return Err(RejectReason::UnknownAuthor);
    }

    let content = event.content.trim();
    if content.is_empty() {
        return Err(RejectReason::Empty);
    }
    if content.starts_with(SYSTEM_PREFIX) {
        return Err(RejectReason::SystemMessage);
    }

    if ctx.speak_only_user_directed {
        let mut p_tags = event.tags.iter().filter(|t| t.kind() == "p").peekable();
        // Only filter messages that actually address someone by p-tag. A
        // plain reply (no p-tags) is for the user and stays speakable.
        if p_tags.peek().is_some() {
            let own_hex = ctx.own_pubkey.to_hex();
            let addresses_user = p_tags.any(|t| t.content() == Some(own_hex.as_str()));
            if !addresses_user {
                return Err(RejectReason::NotAddressedToUser);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::event::{EventBuilder, FinalizeEvent, Tag};
    use nostr::key::Keys;

    struct Fixture {
        channel_id: Uuid,
        other_channel_id: Uuid,
        own: Keys,
        agent: Keys,
        other_agent: Keys,
        stranger: Keys,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                channel_id: Uuid::new_v4(),
                other_channel_id: Uuid::new_v4(),
                own: Keys::generate(),
                agent: Keys::generate(),
                other_agent: Keys::generate(),
                stranger: Keys::generate(),
            }
        }

        fn event(&self, signer: &Keys, kind: u16, channel: Uuid, content: &str) -> Event {
            EventBuilder::new(Kind::Custom(kind), content)
                .tag(Tag::parse(["h", &channel.to_string()]).unwrap())
                .finalize(signer)
                .unwrap()
        }

        /// Like [`Self::event`] but p-tags each pubkey in `mentions` --
        /// used to exercise the user-directed speech filter.
        fn event_mentioning(
            &self,
            signer: &Keys,
            channel: Uuid,
            content: &str,
            mentions: &[PublicKey],
        ) -> Event {
            let mut b = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE), content)
                .tag(Tag::parse(["h", &channel.to_string()]).unwrap());
            for pk in mentions {
                b = b.tag(Tag::parse(["p", &pk.to_hex()]).unwrap());
            }
            b.finalize(signer).unwrap()
        }
    }

    #[test]
    fn user_directed_filter_speaks_reply_that_mentions_the_user() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: true,
        };
        // Coordinator relaying an answer back to the user.
        let event = f.event_mentioning(
            &f.agent,
            f.channel_id,
            "Jupiter is the largest planet.",
            &[f.own.public_key()],
        );
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn user_directed_filter_speaks_untagged_reply() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: true,
        };
        // A plain "on it" acknowledgement with no p-tags is for the user.
        let event = f.event(
            &f.agent,
            KIND_STREAM_MESSAGE,
            f.channel_id,
            "On it, checking now.",
        );
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn user_directed_filter_suppresses_delegation_to_a_teammate() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: true,
        };
        // Coordinator delegating to a teammate, not addressing the user.
        let event = f.event_mentioning(
            &f.agent,
            f.channel_id,
            "who played Han Solo?",
            &[f.other_agent.public_key()],
        );
        assert_eq!(
            is_speakable(&event, &ctx),
            Err(RejectReason::NotAddressedToUser)
        );
    }

    #[test]
    fn user_directed_filter_off_speaks_delegation() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event_mentioning(
            &f.agent,
            f.channel_id,
            "who played Han Solo?",
            &[f.other_agent.public_key()],
        );
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn accepts_a_well_formed_kind9_reply() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(&f.agent, KIND_STREAM_MESSAGE, f.channel_id, "hello there");
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn accepts_kind_40002_rich_content() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(
            &f.agent,
            KIND_STREAM_MESSAGE_V2,
            f.channel_id,
            "hello there",
        );
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn accepts_when_author_is_any_configured_agent() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key(), f.other_agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(&f.other_agent, KIND_STREAM_MESSAGE, f.channel_id, "hi");
        assert_eq!(is_speakable(&event, &ctx), Ok(()));
    }

    #[test]
    fn rejects_wrong_kind() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        // kind:1 (global text note) instead of a stream message.
        let event = f.event(&f.agent, 1, f.channel_id, "hello there");
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::WrongKind));
    }

    #[test]
    fn rejects_missing_h_tag() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE), "hi")
            .finalize(&f.agent)
            .unwrap();
        assert_eq!(
            is_speakable(&event, &ctx),
            Err(RejectReason::ChannelMismatch)
        );
    }

    #[test]
    fn rejects_mismatched_h_tag() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(
            &f.agent,
            KIND_STREAM_MESSAGE,
            f.other_channel_id,
            "hello there",
        );
        assert_eq!(
            is_speakable(&event, &ctx),
            Err(RejectReason::ChannelMismatch)
        );
    }

    #[test]
    fn rejects_self_authored() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            // Deliberately include our own pubkey in the agent list too --
            // self-authorship must still win over "known agent".
            agent_pubkeys: &[f.own.public_key(), f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(&f.own, KIND_STREAM_MESSAGE, f.channel_id, "echo");
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::SelfAuthored));
    }

    #[test]
    fn rejects_unknown_author() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(&f.stranger, KIND_STREAM_MESSAGE, f.channel_id, "hi");
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::UnknownAuthor));
    }

    #[test]
    fn rejects_empty_content() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(&f.agent, KIND_STREAM_MESSAGE, f.channel_id, "   ");
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::Empty));
    }

    #[test]
    fn rejects_system_message_prefix() {
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = f.event(
            &f.agent,
            KIND_STREAM_MESSAGE,
            f.channel_id,
            "[System] agent joined",
        );
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::SystemMessage));
    }

    #[test]
    fn checks_kind_before_channel() {
        // An event that's both the wrong kind AND missing its h tag should
        // report the kind problem -- kind is checked first.
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()],
            speak_only_user_directed: false,
        };
        let event = EventBuilder::new(Kind::Custom(1), "hi")
            .finalize(&f.agent)
            .unwrap();
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::WrongKind));
    }

    #[test]
    fn self_authored_takes_priority_over_unknown_author_check_ordering() {
        // Self-authorship is checked before the general unknown-author
        // case, even though an unlisted own-pubkey would also fail the
        // unknown-author check -- assert the *specific* reason reported.
        let f = Fixture::new();
        let ctx = EligibilityContext {
            channel_id: f.channel_id,
            own_pubkey: f.own.public_key(),
            agent_pubkeys: &[f.agent.public_key()], // own pubkey NOT listed
            speak_only_user_directed: false,
        };
        let event = f.event(&f.own, KIND_STREAM_MESSAGE, f.channel_id, "echo");
        assert_eq!(is_speakable(&event, &ctx), Err(RejectReason::SelfAuthored));
    }
}
