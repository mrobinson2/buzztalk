//! The background WebSocket transport.
//!
//! Runs entirely on its own thread (spawned by [`crate::BuzzAgent::connect`])
//! so [`buzztalk_pipeline::AgentBackend::submit`] never touches a socket.
//! One call to [`run`] owns the connection for the lifetime of the
//! [`crate::BuzzAgent`] that spawned it: connect, NIP-42 authenticate,
//! subscribe to the channel, then loop publishing submitted turns and
//! forwarding speakable inbound replies -- reconnecting with exponential
//! backoff (plus jitter) on any failure, and retrying indefinitely. There
//! is no failure this loop treats as fatal short of an explicit shutdown:
//! relay unreachable, auth rejected, publish rejected, or the connection
//! simply dropping mid-session all take the same path -- log, back off,
//! retry.

use std::net::TcpStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use nostr::event::{Event, EventBuilder, FinalizeEvent, Kind};
use nostr::filter::{Filter, SingleLetterTag};
use nostr::key::{Keys, PublicKey};
use nostr::message::{ClientMessage, RelayMessage, SubscriptionId};
use nostr::nips::nip42::{is_valid_auth_event, ClientAuthentication};
use nostr::types::RelayUrl;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::agent::Command;
use crate::config::{BuzzConfig, ReconnectPolicy};
use crate::eligibility::{self, EligibilityContext};
use crate::events;
use crate::kinds::{KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2};

/// How long a single `ws.read()` call blocks before returning control to
/// the loop so it can check for outbound commands and the shutdown signal.
/// Short enough that `submit`'s text reaches the socket promptly and a
/// shutdown request isn't kept waiting; long enough not to spin the thread.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How long [`await_challenge`] and [`await_auth_ok`] will wait for the
/// relay's half of the NIP-42 handshake before giving up on this attempt.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// The transport thread's entry point. Never returns until
/// [`Command::Shutdown`] is received (or every sender is dropped, which
/// [`crate::BuzzAgent::drop`] treats the same way).
pub(crate) fn run(
    config: BuzzConfig,
    keys: Keys,
    cmd_rx: Receiver<Command>,
    reply_tx: Sender<String>,
) {
    let mut backoff = config.reconnect.initial;
    // Text queued by a `Publish` command that arrived while disconnected
    // (or before the first connection attempt) -- flushed as soon as the
    // next session authenticates. Only the most recent submission is kept:
    // `buzztalk-session`'s single-turn model means a newer submission
    // supersedes an older one anyway (see `BuzzAgent`'s turn-attribution
    // docs), so there is nothing to gain from queuing more than one.
    let mut pending: Option<String> = None;

    loop {
        match run_session(
            &config,
            &keys,
            &cmd_rx,
            &reply_tx,
            &mut pending,
            &mut backoff,
        ) {
            SessionOutcome::Shutdown => return,
            SessionOutcome::Disconnected(reason) => {
                eprintln!(
                    "buzztalk-buzz: relay session ended ({reason}); reconnecting in {backoff:?}"
                );
            }
        }

        if wait_or_shutdown(&cmd_rx, backoff, &mut pending) {
            return;
        }
        backoff = next_backoff(backoff, &config.reconnect);
    }
}

enum SessionOutcome {
    Shutdown,
    Disconnected(String),
}

/// One connect-authenticate-subscribe-serve attempt. Returns as soon as
/// the session ends, for any reason.
fn run_session(
    config: &BuzzConfig,
    keys: &Keys,
    cmd_rx: &Receiver<Command>,
    reply_tx: &Sender<String>,
    pending: &mut Option<String>,
    backoff: &mut Duration,
) -> SessionOutcome {
    // Fast exit if a shutdown (or a fresh submission to remember) is
    // already waiting, before spending time on a connection attempt.
    match cmd_rx.try_recv() {
        Ok(Command::Shutdown) => return SessionOutcome::Shutdown,
        Ok(Command::Publish(text)) => *pending = Some(text),
        Err(_) => {}
    }

    let relay_url = match RelayUrl::parse(&config.relay_url) {
        Ok(u) => u,
        Err(e) => return SessionOutcome::Disconnected(format!("invalid relay URL: {e}")),
    };

    let mut socket = match connect(config.relay_url.as_str()) {
        Ok((socket, _response)) => socket,
        Err(e) => return SessionOutcome::Disconnected(format!("connect failed: {e}")),
    };
    eprintln!("buzztalk-buzz: connected to {}", config.relay_url);

    if let Err(e) = set_read_timeout(&socket, Some(READ_POLL_INTERVAL)) {
        return SessionOutcome::Disconnected(format!("failed to configure socket: {e}"));
    }

    // NIP-42: Buzz sends the AUTH challenge proactively on connect.
    let challenge = match await_challenge(&mut socket) {
        Ok(c) => c,
        Err(e) => return SessionOutcome::Disconnected(format!("auth challenge: {e}")),
    };

    let auth_event = match sign_auth_event(keys, &challenge, relay_url.clone()) {
        Ok(ev) => ev,
        Err(e) => return SessionOutcome::Disconnected(format!("failed to sign auth event: {e}")),
    };
    debug_assert!(
        is_valid_auth_event(&auth_event, &relay_url, &challenge),
        "the event we just built must satisfy NIP-42's own validity check"
    );
    if let Err(e) = send(&mut socket, ClientMessage::auth(auth_event)) {
        return SessionOutcome::Disconnected(format!("failed to send auth: {e}"));
    }

    if let Err(e) = await_auth_ok(&mut socket) {
        return SessionOutcome::Disconnected(format!("auth rejected: {e}"));
    }
    eprintln!(
        "buzztalk-buzz: authenticated as {}",
        keys.public_key().to_hex()
    );
    // This session is healthy: reset backoff so a *future* hiccup, after
    // however long this session lasts, starts from the initial delay
    // rather than wherever a previous run of failures had grown it to.
    *backoff = config.reconnect.initial;

    let sub_id = SubscriptionId::generate();
    let filter = Filter::new()
        .kinds([
            Kind::Custom(KIND_STREAM_MESSAGE),
            Kind::Custom(KIND_STREAM_MESSAGE_V2),
        ])
        .custom_tag(SingleLetterTag::LOWERCASE_H, config.channel_id.to_string());
    if let Err(e) = send(&mut socket, ClientMessage::req(sub_id, [filter])) {
        return SessionOutcome::Disconnected(format!("failed to subscribe: {e}"));
    }

    // Flush whatever was queued while disconnected (or before the very
    // first connection) now that we can actually publish.
    if let Some(text) = pending.take() {
        publish(&mut socket, config, keys, &text);
    }

    serve(&mut socket, config, keys, cmd_rx, reply_tx)
}

/// The steady-state loop once connected, authenticated, and subscribed:
/// alternately drain outbound commands and read inbound relay messages
/// until something ends the session.
fn serve(
    socket: &mut Socket,
    config: &BuzzConfig,
    keys: &Keys,
    cmd_rx: &Receiver<Command>,
    reply_tx: &Sender<String>,
) -> SessionOutcome {
    let own_pubkey = keys.public_key();
    loop {
        match cmd_rx.try_recv() {
            Ok(Command::Shutdown) => {
                let _ = socket.close(None);
                return SessionOutcome::Shutdown;
            }
            Ok(Command::Publish(text)) => publish(socket, config, keys, &text),
            Err(TryRecvError::Empty) => {}
            // The `BuzzAgent` was dropped without an explicit Shutdown
            // (e.g. process exit) -- same effect, since nothing will ever
            // send on this channel again.
            Err(TryRecvError::Disconnected) => {
                let _ = socket.close(None);
                return SessionOutcome::Shutdown;
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                handle_relay_text(&text, config, &own_pubkey, reply_tx);
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) => {
                return SessionOutcome::Disconnected("relay closed the connection".to_string())
            }
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {} // just our read-poll interval elapsing
            Err(e) => return SessionOutcome::Disconnected(format!("read error: {e}")),
        }
    }
}

fn handle_relay_text(
    text: &str,
    config: &BuzzConfig,
    own_pubkey: &PublicKey,
    reply_tx: &Sender<String>,
) {
    let Ok(msg) = RelayMessage::from_json(text) else {
        return; // Not a message shape we understand -- ignore rather than fail the session.
    };
    match msg {
        RelayMessage::Event { event, .. } => {
            let ctx = EligibilityContext {
                channel_id: config.channel_id,
                own_pubkey: *own_pubkey,
                agent_pubkeys: &config.agent_pubkeys,
                speak_only_user_directed: config.speak_only_user_directed,
            };
            if eligibility::is_speakable(&event, &ctx).is_ok() {
                let _ = reply_tx.send(event.content.clone());
            }
            // A rejection here is the expected, frequent case (most
            // channel traffic isn't a speakable agent reply) -- not logged
            // per-event to avoid drowning real signal in channel noise.
        }
        RelayMessage::Notice(notice) => {
            eprintln!("buzztalk-buzz: relay notice: {notice}");
        }
        RelayMessage::Closed { message, .. } => {
            eprintln!("buzztalk-buzz: subscription closed by relay: {message}");
        }
        RelayMessage::Ok {
            status: false,
            message,
            ..
        } => {
            eprintln!("buzztalk-buzz: relay rejected a published event: {message}");
        }
        // A relay-initiated re-auth challenge mid-session, or any other
        // message this loop doesn't act on. Re-authentication mid-session
        // is not implemented -- see this crate's report for why that's an
        // accepted, documented gap rather than a silent one.
        _ => {}
    }
}

fn publish(socket: &mut Socket, config: &BuzzConfig, keys: &Keys, text: &str) {
    let builder = match events::build_stream_message(config.channel_id, text, &config.agent_pubkeys)
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("buzztalk-buzz: refusing to publish turn submission: {e}");
            return;
        }
    };
    let event = match sign(builder, keys) {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("buzztalk-buzz: failed to sign turn submission: {e}");
            return;
        }
    };
    if let Err(e) = send(socket, ClientMessage::event(event)) {
        eprintln!("buzztalk-buzz: failed to publish turn submission: {e}");
    }
}

fn sign_auth_event(
    keys: &Keys,
    challenge: &str,
    relay_url: RelayUrl,
) -> Result<Event, nostr::error::Error> {
    ClientAuthentication::new(challenge, relay_url).finalize(keys)
}

fn sign(builder: EventBuilder, keys: &Keys) -> Result<Event, nostr::error::Error> {
    builder.finalize(keys)
}

// `tungstenite::Error` is a large enum; every caller here immediately
// formats and discards it (logged, never stored or propagated further), so
// boxing it would only add an allocation on a path that's already about to
// throw the value away.
#[allow(clippy::result_large_err)]
fn send(socket: &mut Socket, message: ClientMessage<'_>) -> tungstenite::Result<()> {
    socket.send(Message::Text(message.as_json()))
}

/// Block (respecting [`HANDSHAKE_TIMEOUT`]) for the relay's proactive
/// NIP-42 `["AUTH", <challenge>]`, ignoring anything else that arrives
/// first.
fn await_challenge(socket: &mut Socket) -> Result<String, String> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(RelayMessage::Auth { challenge }) = RelayMessage::from_json(&text) {
                    return Ok(challenge.into_owned());
                }
            }
            Ok(Message::Close(_)) => {
                return Err("relay closed the connection before sending a challenge".to_string())
            }
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("timed out waiting for an auth challenge".to_string())
}

/// Block (respecting [`HANDSHAKE_TIMEOUT`]) for the relay's `OK` response to
/// our `AUTH` event.
fn await_auth_ok(socket: &mut Socket) -> Result<(), String> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(RelayMessage::Ok {
                    status, message, ..
                }) = RelayMessage::from_json(&text)
                {
                    return if status {
                        Ok(())
                    } else {
                        Err(message.into_owned())
                    };
                }
            }
            Ok(Message::Close(_)) => {
                return Err("relay closed the connection during authentication".to_string())
            }
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("timed out waiting for auth confirmation".to_string())
}

/// Sleep for `duration`, but wake early (and report whether to shut down)
/// if a command arrives. `Publish` commands received while waiting to
/// reconnect are captured into `pending` rather than lost.
fn wait_or_shutdown(
    cmd_rx: &Receiver<Command>,
    duration: Duration,
    pending: &mut Option<String>,
) -> bool {
    let deadline = Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match cmd_rx.recv_timeout(remaining) {
            Ok(Command::Shutdown) => return true,
            Ok(Command::Publish(text)) => *pending = Some(text),
            Err(RecvTimeoutError::Timeout) => return false,
            Err(RecvTimeoutError::Disconnected) => return true,
        }
    }
}

/// Grow `current` by [`ReconnectPolicy::multiplier`], cap it at
/// [`ReconnectPolicy::max`], then randomize by +/-[`ReconnectPolicy::jitter`]
/// so many instances backing off from the same outage don't all reconnect
/// in lockstep.
fn next_backoff(current: Duration, policy: &ReconnectPolicy) -> Duration {
    let grown = current.mul_f64(policy.multiplier).min(policy.max);
    let jitter_fraction = (rand::random::<f64>() * 2.0 - 1.0) * policy.jitter;
    let jittered = grown.mul_f64((1.0 + jitter_fraction).max(0.0));
    jittered.clamp(policy.initial, policy.max)
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    matches!(
        e,
        tungstenite::Error::Io(io_err)
            if matches!(io_err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
    )
}

fn set_read_timeout(ws: &Socket, timeout: Option<Duration>) -> std::io::Result<()> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(s) => s.get_ref().set_read_timeout(timeout),
        // `#[non_exhaustive]`, and other backends aren't enabled by this
        // crate's feature flags -- nothing to configure.
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_by_the_multiplier_and_caps_at_max() {
        let policy = ReconnectPolicy {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(2),
            multiplier: 2.0,
            jitter: 0.0, // deterministic for this test
        };
        let mut backoff = policy.initial;
        for _ in 0..10 {
            backoff = next_backoff(backoff, &policy);
            assert!(backoff <= policy.max);
        }
        assert_eq!(backoff, policy.max, "must eventually saturate at max");
    }

    #[test]
    fn backoff_never_drops_below_initial_even_with_negative_jitter() {
        let policy = ReconnectPolicy {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.5,
        };
        for _ in 0..200 {
            let backoff = next_backoff(policy.initial, &policy);
            assert!(backoff >= policy.initial);
            assert!(backoff <= policy.max);
        }
    }

    #[test]
    fn backoff_with_zero_jitter_is_exactly_the_multiplier() {
        let policy = ReconnectPolicy {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(60),
            multiplier: 3.0,
            jitter: 0.0,
        };
        let next = next_backoff(Duration::from_millis(200), &policy);
        assert_eq!(next, Duration::from_millis(600));
    }
}
