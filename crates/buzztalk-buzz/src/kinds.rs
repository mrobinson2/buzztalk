//! Buzz event-kind constants this crate speaks.
//!
//! Ported from Buzz's `buzz-core` kind registry
//! (`~/.buzztalk/reference/buzz-kinds.rs`, Apache-2.0, Copyright Block,
//! Inc. -- see the workspace-root `NOTICE`). That registry defines close to
//! 150 kinds; only the two BuzzTalk ever publishes or listens for are
//! reproduced here rather than depended on, since `buzz-core` is reference
//! material, not a published or workspace-local crate, and nothing else in
//! this crate needs the rest of the registry.

/// NIP-29 group chat message kind. What [`crate::events::build_stream_message`]
/// publishes, and one of the two kinds [`crate::eligibility::is_speakable`]
/// accepts as an inbound agent reply.
///
/// Matches `buzz-kinds.rs`'s `KIND_STREAM_MESSAGE`.
pub const KIND_STREAM_MESSAGE: u16 = 9;

/// Buzz's "rich content" stream message variant. BuzzTalk never publishes
/// this itself, but per this crate's task brief and `NOSTR.md`'s kind
/// table, an inbound message of this kind is an equally valid agent reply.
///
/// Matches `buzz-kinds.rs`'s `KIND_STREAM_MESSAGE_V2`.
pub const KIND_STREAM_MESSAGE_V2: u16 = 40_002;
