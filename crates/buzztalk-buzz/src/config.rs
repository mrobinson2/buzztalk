//! Configuration and key sourcing for [`crate::BuzzAgent`].

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use nostr::key::{Keys, PublicKey};
use uuid::Uuid;

use crate::error::BuzzError;

/// Default gap with no further chunks from the agent before
/// [`crate::BuzzAgent`] treats a reply as finished. See
/// [`crate::agent::BuzzAgent`]'s docs for why this is a heuristic timeout,
/// not a protocol signal.
pub const DEFAULT_QUIET_PERIOD: Duration = Duration::from_millis(1200);

/// Where the Nostr secret key used to authenticate with the relay comes
/// from. The key is never hard-coded and never logged: [`KeySource::load`]
/// reads it, and every error path deliberately omits the key material and
/// the underlying parser's message, both of which risk echoing it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// Read from the named environment variable. Accepts hex or bech32
    /// `nsec1...`. [`KeySource::default_env`] is the conventional choice
    /// (`BUZZTALK_NOSTR_SECRET`).
    EnvVar(String),
    /// Read from a file containing the key (hex or bech32 `nsec1...`),
    /// trimmed of surrounding whitespace before parsing.
    File(PathBuf),
    /// The key material itself, already resolved by the caller (e.g. read
    /// from a secret manager upstream). Exists for tests and advanced
    /// embedding -- prefer [`KeySource::EnvVar`] or [`KeySource::File`]
    /// everywhere else so the key never has to pass through argv or shell
    /// history to get here.
    Literal(String),
}

/// The environment variable [`KeySource::default_env`] names.
pub const DEFAULT_KEY_ENV_VAR: &str = "BUZZTALK_NOSTR_SECRET";

impl KeySource {
    /// [`KeySource::EnvVar`] naming [`DEFAULT_KEY_ENV_VAR`].
    pub fn default_env() -> Self {
        KeySource::EnvVar(DEFAULT_KEY_ENV_VAR.to_string())
    }

    /// Resolve this source into a signing [`Keys`] pair.
    pub fn load(&self) -> Result<Keys, BuzzError> {
        let raw = match self {
            KeySource::EnvVar(name) => env::var(name).map_err(|_| {
                BuzzError::KeyUnavailable(format!("environment variable {name:?} is not set"))
            })?,
            KeySource::File(path) => fs::read_to_string(path)
                .map_err(|e| BuzzError::KeyUnavailable(format!("reading {path:?}: {e}")))?
                .trim()
                .to_string(),
            KeySource::Literal(s) => s.clone(),
        };
        // Deliberately swallow the underlying parse error: `nostr`'s error
        // message is not guaranteed never to include a fragment of the
        // input it failed to parse, and this is exactly the code path that
        // handles a mistyped or malformed secret key.
        Keys::parse(raw.trim()).map_err(|_| BuzzError::InvalidKey)
    }
}

/// How [`crate::transport`] backs off between reconnect attempts.
///
/// Backoff resets to [`ReconnectPolicy::initial`] every time a session
/// successfully authenticates, so a relay that comes back after a long
/// outage isn't punished with a maxed-out delay on its *next* hiccup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconnectPolicy {
    /// Delay before the first reconnect attempt, and the delay after every
    /// successful authentication.
    pub initial: Duration,
    /// Backoff never grows past this.
    pub max: Duration,
    /// Multiplier applied to the current backoff after each failed attempt.
    pub multiplier: f64,
    /// Fraction of the computed backoff to randomize by, in both
    /// directions (e.g. `0.2` = +/-20%), so many BuzzTalk instances backing
    /// off from the same relay outage don't reconnect in lockstep.
    pub jitter: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.2,
        }
    }
}

/// Everything [`crate::BuzzAgent::connect`] needs: which relay, which
/// channel, which key, which pubkeys count as "the agent", and the tuning
/// knobs for turn completion and reconnection.
#[derive(Debug, Clone)]
pub struct BuzzConfig {
    /// The relay's WebSocket URL, e.g. `wss://relay.example.com`.
    pub relay_url: String,
    /// The Buzz channel (NIP-29 group) BuzzTalk publishes into and
    /// subscribes from -- goes on the wire as the event's `h` tag.
    pub channel_id: Uuid,
    /// Where to load the Nostr secret key BuzzTalk signs events with.
    pub key_source: KeySource,
    /// Pubkeys BuzzTalk treats as "the agent" for this channel: every
    /// submitted turn is p-tagged with all of them, and
    /// [`crate::eligibility::is_speakable`] only accepts inbound replies
    /// authored by one of them.
    ///
    /// Fixed for the lifetime of one [`crate::BuzzAgent`] -- there is no
    /// live membership refresh (unlike Buzz's own desktop client, which
    /// re-polls channel membership; see this crate's report for why that
    /// was judged out of scope here).
    pub agent_pubkeys: Vec<PublicKey>,
    /// How long with no further reply text before a turn is considered
    /// complete. See [`crate::agent::BuzzAgent`]'s docs.
    pub reply_quiet_period: Duration,
    /// Reconnect backoff tuning.
    pub reconnect: ReconnectPolicy,
}

impl BuzzConfig {
    /// A config with the given required fields and every tunable at its
    /// documented default. No agent pubkeys are configured yet -- without
    /// at least one, [`crate::eligibility::is_speakable`] rejects every
    /// inbound event, so callers almost always want
    /// [`BuzzConfig::with_agent_pubkeys`] too.
    pub fn new(relay_url: impl Into<String>, channel_id: Uuid, key_source: KeySource) -> Self {
        Self {
            relay_url: relay_url.into(),
            channel_id,
            key_source,
            agent_pubkeys: Vec::new(),
            reply_quiet_period: DEFAULT_QUIET_PERIOD,
            reconnect: ReconnectPolicy::default(),
        }
    }

    /// Set the agent pubkeys (see [`BuzzConfig::agent_pubkeys`]).
    pub fn with_agent_pubkeys(mut self, agent_pubkeys: Vec<PublicKey>) -> Self {
        self.agent_pubkeys = agent_pubkeys;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::nips::nip19::ToBech32;

    #[test]
    fn env_var_source_loads_hex_key() {
        let key = Keys::generate();
        let hex = key.secret_key().to_secret_hex();
        // Rust runs unit tests in one process with a shared environment,
        // so scope the var name to this test to avoid clobbering a
        // concurrently-running test's own env-based case.
        let var = "BUZZTALK_TEST_ENV_VAR_HEX";
        // SAFETY: no other thread reads/writes this specific, test-scoped
        // variable name concurrently.
        unsafe { env::set_var(var, &hex) };
        let loaded = KeySource::EnvVar(var.to_string()).load().unwrap();
        unsafe { env::remove_var(var) };
        assert_eq!(loaded.public_key(), key.public_key());
    }

    #[test]
    fn env_var_source_loads_bech32_nsec_key() {
        let key = Keys::generate();
        let nsec = key.secret_key().to_bech32().unwrap();
        let var = "BUZZTALK_TEST_ENV_VAR_NSEC";
        unsafe { env::set_var(var, &nsec) };
        let loaded = KeySource::EnvVar(var.to_string()).load().unwrap();
        unsafe { env::remove_var(var) };
        assert_eq!(loaded.public_key(), key.public_key());
    }

    #[test]
    fn env_var_source_reports_missing_var_without_panicking() {
        let err = KeySource::EnvVar("BUZZTALK_TEST_DEFINITELY_UNSET".to_string()).load();
        assert!(matches!(err, Err(BuzzError::KeyUnavailable(_))));
    }

    #[test]
    fn file_source_loads_and_trims_whitespace() {
        let key = Keys::generate();
        let hex = key.secret_key().to_secret_hex();
        let path =
            std::env::temp_dir().join(format!("buzztalk-test-key-{}.txt", std::process::id()));
        fs::write(&path, format!("  {hex}\n")).unwrap();
        let loaded = KeySource::File(path.clone()).load().unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(loaded.public_key(), key.public_key());
    }

    #[test]
    fn file_source_reports_missing_file() {
        let path = PathBuf::from("/nonexistent/buzztalk-test-key-that-does-not-exist.txt");
        let err = KeySource::File(path).load();
        assert!(matches!(err, Err(BuzzError::KeyUnavailable(_))));
    }

    #[test]
    fn literal_source_rejects_garbage_without_leaking_it_in_the_error() {
        let err = KeySource::Literal("not a key".to_string()).load();
        let Err(e) = err else {
            panic!("expected an error")
        };
        assert!(matches!(e, BuzzError::InvalidKey));
        assert!(!e.to_string().contains("not a key"));
    }

    #[test]
    fn default_env_names_the_documented_variable() {
        assert_eq!(
            KeySource::default_env(),
            KeySource::EnvVar(DEFAULT_KEY_ENV_VAR.to_string())
        );
        assert_eq!(DEFAULT_KEY_ENV_VAR, "BUZZTALK_NOSTR_SECRET");
    }

    #[test]
    fn config_new_defaults_have_no_agent_pubkeys() {
        let cfg = BuzzConfig::new(
            "wss://relay.example.com",
            Uuid::nil(),
            KeySource::default_env(),
        );
        assert!(cfg.agent_pubkeys.is_empty());
        assert_eq!(cfg.reply_quiet_period, DEFAULT_QUIET_PERIOD);
    }

    #[test]
    fn reconnect_policy_default_resets_are_sane() {
        let policy = ReconnectPolicy::default();
        assert!(policy.initial < policy.max);
        assert!(policy.multiplier > 1.0);
        assert!(policy.jitter >= 0.0 && policy.jitter < 1.0);
    }
}
