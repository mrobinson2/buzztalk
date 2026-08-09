//! Outbound kind:9 event construction.
//!
//! [`build_stream_message`] is a narrowed port of `buzz-sdk`'s
//! `build_message` builder
//! (`~/.buzztalk/reference/buzz-sdk/src/builders.rs`, function
//! `build_message`; Apache-2.0, Copyright Block, Inc. -- see the
//! workspace-root `NOTICE`). It is reproduced here rather than depended on
//! because `buzz-sdk` is reference material on disk, not a published or
//! workspace-local crate this workspace can add as a path dependency.
//!
//! Only what a plain turn submission needs survives the port: the channel
//! `h` tag, the same 64 KiB content-length guard, and deduplicated agent
//! `p` tags. `build_message`'s `thread_ref` (NIP-10 reply markers),
//! `broadcast` tag, and `media_tags` (imeta) parameters are all dropped --
//! BuzzTalk never threads, broadcasts, or attaches media to a turn
//! submission, and carrying that surface here would just be unused
//! complexity this crate would have to keep in sync with an SDK it can't
//! even depend on.

use std::collections::HashSet;

use nostr::event::{EventBuilder, Kind, Tag};
use nostr::key::PublicKey;
use uuid::Uuid;

use crate::error::BuzzError;
use crate::kinds::KIND_STREAM_MESSAGE;

/// Same cap `buzz-sdk::build_message` enforces on `content`.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

/// Build (but do not sign) a kind:9 stream message publishing `content`
/// into `channel_id`, p-tagging every pubkey in `agent_pubkeys`
/// (deduplicated, first-seen order preserved).
///
/// Returns [`BuzzError::EventBuild`] if `content` is empty or exceeds
/// [`MAX_CONTENT_BYTES`] -- both are checked here, before signing, so a bad
/// submission never reaches the network.
pub fn build_stream_message(
    channel_id: Uuid,
    content: &str,
    agent_pubkeys: &[PublicKey],
) -> Result<EventBuilder, BuzzError> {
    if content.is_empty() {
        return Err(BuzzError::EventBuild(
            "content must not be empty".to_string(),
        ));
    }
    if content.len() > MAX_CONTENT_BYTES {
        return Err(BuzzError::EventBuild(format!(
            "content exceeds maximum size of {MAX_CONTENT_BYTES} bytes (got {})",
            content.len()
        )));
    }

    let mut tags = vec![tag(&["h", &channel_id.to_string()])?];

    let mut seen = HashSet::with_capacity(agent_pubkeys.len());
    for pk in agent_pubkeys {
        if seen.insert(*pk) {
            tags.push(tag(&["p", &pk.to_hex()])?);
        }
    }

    Ok(EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE), content).tags(tags))
}

/// Parse a tag, mapping the (infallible-in-practice, since every caller
/// here supplies non-empty literal slices) error the same way
/// `buzz-sdk::builders::tag` does.
fn tag(parts: &[&str]) -> Result<Tag, BuzzError> {
    Tag::parse(parts.iter().copied()).map_err(|e| BuzzError::EventBuild(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::event::FinalizeEvent;
    use nostr::key::Keys;

    #[test]
    fn builds_kind_9_with_h_tag_and_valid_signature() {
        let keys = Keys::generate();
        let channel_id = Uuid::new_v4();
        let builder = build_stream_message(channel_id, "hello", &[]).unwrap();
        let event = builder.finalize(&keys).unwrap();

        assert_eq!(event.kind, Kind::Custom(KIND_STREAM_MESSAGE));
        assert_eq!(event.content, "hello");
        assert_eq!(event.pubkey, keys.public_key());

        let h_tag = event
            .tags
            .iter()
            .find(|t| t.kind() == "h")
            .expect("h tag present");
        assert_eq!(h_tag.content(), Some(channel_id.to_string().as_str()));

        // Verify the signature ourselves rather than trusting `finalize`
        // blindly -- `Event::verify` checks both the id (content hash) and
        // the schnorr signature against the author's pubkey.
        event.verify().expect("signature must verify");
    }

    #[test]
    fn p_tags_every_agent_pubkey() {
        let keys = Keys::generate();
        let agent_a = Keys::generate().public_key();
        let agent_b = Keys::generate().public_key();
        let builder = build_stream_message(Uuid::new_v4(), "hi", &[agent_a, agent_b]).unwrap();
        let event = builder.finalize(&keys).unwrap();

        let p_tags: Vec<PublicKey> = event
            .tags
            .iter()
            .filter(|t| t.kind() == "p")
            .filter_map(|t| t.content())
            .filter_map(|hex| PublicKey::from_hex(hex).ok())
            .collect();
        assert_eq!(p_tags, vec![agent_a, agent_b]);
    }

    #[test]
    fn p_tags_are_deduplicated_preserving_first_seen_order() {
        let keys = Keys::generate();
        let agent_a = Keys::generate().public_key();
        let agent_b = Keys::generate().public_key();
        let builder =
            build_stream_message(Uuid::new_v4(), "hi", &[agent_a, agent_b, agent_a]).unwrap();
        let event = builder.finalize(&keys).unwrap();

        let p_tags: Vec<PublicKey> = event
            .tags
            .iter()
            .filter(|t| t.kind() == "p")
            .filter_map(|t| t.content())
            .filter_map(|hex| PublicKey::from_hex(hex).ok())
            .collect();
        assert_eq!(p_tags, vec![agent_a, agent_b]);
    }

    #[test]
    fn rejects_empty_content() {
        let err = build_stream_message(Uuid::new_v4(), "", &[]).unwrap_err();
        assert!(matches!(err, BuzzError::EventBuild(_)));
    }

    #[test]
    fn rejects_oversized_content() {
        let too_big = "x".repeat(MAX_CONTENT_BYTES + 1);
        let err = build_stream_message(Uuid::new_v4(), &too_big, &[]).unwrap_err();
        assert!(matches!(err, BuzzError::EventBuild(_)));
    }

    #[test]
    fn accepts_content_at_exactly_the_cap() {
        let exactly = "x".repeat(MAX_CONTENT_BYTES);
        let builder = build_stream_message(Uuid::new_v4(), &exactly, &[]).unwrap();
        assert_eq!(builder.content.len(), MAX_CONTENT_BYTES);
    }

    #[test]
    fn different_channels_get_different_h_tags() {
        let keys = Keys::generate();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let event_a = build_stream_message(a, "hi", &[])
            .unwrap()
            .finalize(&keys)
            .unwrap();
        let event_b = build_stream_message(b, "hi", &[])
            .unwrap()
            .finalize(&keys)
            .unwrap();
        let h = |e: &nostr::event::Event| {
            e.tags
                .iter()
                .find(|t| t.kind() == "h")
                .and_then(|t| t.content().map(str::to_string))
                .unwrap()
        };
        assert_ne!(h(&event_a), h(&event_b));
    }
}
