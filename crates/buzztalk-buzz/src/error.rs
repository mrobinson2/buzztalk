//! [`BuzzError`]: everything that can go wrong building a [`crate::BuzzAgent`]
//! or a signed event, before any network I/O is involved.
//!
//! Deliberately excludes connection/auth/publish failures -- those happen on
//! the background transport thread, after construction, where there is no
//! caller left to hand a `Result` to; see [`crate::agent`]'s docs for how
//! those are surfaced instead (logged, not returned).

/// Errors returned by fallible, synchronous [`crate::BuzzAgent`] setup.
#[derive(Debug, thiserror::Error)]
pub enum BuzzError {
    /// The configured [`crate::KeySource`] could not be read at all (env var
    /// unset, file missing/unreadable).
    #[error("relay signing key unavailable: {0}")]
    KeyUnavailable(String),

    /// The key material was readable but is not a valid Nostr secret key
    /// (wrong length, invalid hex/bech32). Deliberately does not echo the
    /// input or the underlying parser's message -- both risk leaking key
    /// material into logs.
    #[error("configured secret key is not a valid Nostr secret key (hex or nsec1...)")]
    InvalidKey,

    /// The configured relay URL is not a valid `ws://`/`wss://` URL.
    #[error("invalid relay URL {0:?}: {1}")]
    InvalidRelayUrl(String, String),

    /// A submitted turn's text failed validation before it could even be
    /// turned into an event (e.g. too large, empty).
    #[error("cannot build turn submission event: {0}")]
    EventBuild(String),

    /// The background transport thread failed to spawn.
    #[error("failed to spawn Buzz transport thread: {0}")]
    Spawn(String),
}
