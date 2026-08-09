//! `buzztalk-buzz`: the [`buzztalk_pipeline::AgentBackend`] that makes
//! BuzzTalk talk to a real Buzz relay.
//!
//! Everything else in the workspace works against
//! [`buzztalk_pipeline::EchoAgent`], a local stub. This crate is the real
//! implementation: [`BuzzAgent`] opens a NIP-42-authenticated WebSocket to a
//! Buzz relay, publishes each submitted turn as a signed kind:9 event, and
//! turns speakable inbound replies back into [`buzztalk_pipeline::AgentEvent`]s.
//!
//! # Module map
//!
//! * [`config`] -- [`BuzzConfig`] and [`KeySource`] (how the relay, channel,
//!   and signing key are configured; the key is never hard-coded or logged).
//! * [`kinds`] -- the two Buzz event kinds this crate speaks.
//! * [`events`] -- outbound kind:9 construction (a narrowed port of
//!   `buzz-sdk`'s builder; see that module's docs for attribution).
//! * [`eligibility`] -- the inbound "is this a speakable agent reply?"
//!   classifier, reused from Buzz's own rules as specified in this crate's
//!   task brief (see that module's docs for what "reused" means here).
//! * [`transport`] (private) -- the background thread: connect, NIP-42
//!   auth, subscribe, publish, reconnect-with-backoff. Never touched by the
//!   orchestrator thread.
//! * [`agent`] -- [`BuzzAgent`] itself: the public [`AgentBackend`] impl,
//!   and the turn-attribution logic described in its docs.
//!
//! # Attribution
//!
//! Buzz is Apache-2.0, Copyright Block, Inc. (see the workspace-root
//! `NOTICE` and `LICENSE`, and `buzz-kinds.rs` /
//! `~/.buzztalk/reference/buzz-sdk` for the originals). Where this crate
//! ports logic rather than reimplementing it from the wire spec, the
//! porting module says so and cites its source file.

#![warn(missing_docs)]

mod agent;
mod config;
mod eligibility;
mod error;
mod events;
mod kinds;
mod transport;

pub use agent::BuzzAgent;
pub use config::{
    BuzzConfig, KeySource, ReconnectPolicy, DEFAULT_KEY_ENV_VAR, DEFAULT_QUIET_PERIOD,
};
pub use eligibility::{is_speakable, EligibilityContext, RejectReason, SYSTEM_PREFIX};
pub use error::BuzzError;
pub use events::{build_stream_message, MAX_CONTENT_BYTES};
pub use kinds::{KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2};

/// Re-exported so callers building a [`BuzzConfig`] don't need a direct
/// `nostr` dependency just to name [`nostr::key::PublicKey`] /
/// [`nostr::key::Keys`].
pub use nostr::key::{Keys, PublicKey};
/// Re-exported for the same reason -- [`BuzzConfig::channel_id`]'s type.
pub use uuid::Uuid;
