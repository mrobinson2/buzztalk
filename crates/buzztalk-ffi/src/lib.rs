//! A C ABI over the BuzzTalk conversation pipeline, so a Flutter (or any
//! other non-Rust) app can drive voice on a phone the way `buzztalkd`
//! drives it from a terminal on the desktop.
//!
//! # Why this crate exists
//!
//! `buzztalkd` is a binary: it owns `main`, parses flags, and prints
//! events. A mobile app can't shell out to it — it needs to *call* the
//! engine in-process. This crate is the thin, `#[no_mangle] extern "C"`
//! surface a `dart:ffi` / `flutter_rust_bridge` layer binds to:
//!
//! ```text
//! Flutter (Dart) ── dart:ffi ──► buzztalk-ffi (C ABI) ──► ConversationPipeline
//!                                                          + BuzzAgent (relay)
//! ```
//!
//! # Status
//!
//! This is the **integration scaffold** described in
//! `docs/IOS-VOICE-PORT.md`. It compiles and its API is stable enough to
//! bind against, but the full pipeline it wraps does not yet *link* for
//! `aarch64-apple-ios`: the STT backend (`sherpa-onnx-sys`) needs an
//! iOS-built onnxruntime and a full-Xcode iOS SDK (Command Line Tools are
//! not enough). Until then this crate builds and is exercised on the host.
//! See the port doc for the sequencing.
//!
//! # Threading and safety contract
//!
//! * [`buzztalk_start`] returns an opaque handle. All other calls take that
//!   handle. It is **not** thread-safe: call from one thread (the app's
//!   platform/audio thread), or serialize externally.
//! * [`buzztalk_poll_event`] is non-blocking. The app pumps it on a timer /
//!   run-loop and turns each event into a Dart-side stream item. Strings it
//!   returns are owned by the caller and must be freed with
//!   [`buzztalk_string_free`].
//! * [`buzztalk_stop`] consumes the handle; using it afterwards is
//!   undefined behaviour.
//!
//! The signing key is passed as a path (never the raw key across the FFI
//! boundary) — the app writes the user's key to an app-private file the
//! Rust side reads once, matching `buzztalkd --key-file`.

#![allow(clippy::missing_safety_doc)] // The safety contract is in the module docs.

use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;

use buzztalk_buzz::{BuzzAgent, BuzzConfig, KeySource};
use buzztalk_pipeline::{ConversationPipeline, PipelineConfig, PipelineEvent};

/// Opaque handle to a running session. The app holds this pointer and hands
/// it back to every call; it never inspects the contents.
pub struct BuzztalkSession {
    pipeline: ConversationPipeline,
}

/// Discriminant for [`buzztalk_poll_event`]'s out-param, so the Dart side
/// can `switch` on an int instead of parsing strings.
#[repr(C)]
pub enum BuzztalkEventKind {
    /// No event was ready this poll.
    None = 0,
    /// A partial (in-progress) transcript; text is set.
    Partial = 1,
    /// A finalized transcript; text is set.
    FinalTranscript = 2,
    /// A chunk of the agent's spoken reply; text is set.
    AgentText = 3,
    /// The session's phase changed; text is the state name.
    StateChanged = 4,
    /// The audio engine self-healed (device change / dead capture); text is
    /// the reason.
    AudioRebuilt = 5,
    /// A capability was lost; text is the reason.
    CapabilityLost = 6,
}

/// Start a voice session bound to `relay_url` / `channel_uuid`, signing as
/// the key in `key_file_path`, p-tagging (and speaking the replies of) the
/// single `agent_pubkey_hex`. Returns null on any error.
///
/// All string args are borrowed for the duration of the call only.
#[no_mangle]
pub unsafe extern "C" fn buzztalk_start(
    relay_url: *const c_char,
    channel_uuid: *const c_char,
    agent_pubkey_hex: *const c_char,
    key_file_path: *const c_char,
) -> *mut BuzztalkSession {
    let (Some(relay), Some(channel), Some(agent), Some(key_path)) = (
        cstr(relay_url),
        cstr(channel_uuid),
        cstr(agent_pubkey_hex),
        cstr(key_file_path),
    ) else {
        return ptr::null_mut();
    };

    let Ok(channel_id) = buzztalk_buzz::Uuid::parse_str(&channel) else {
        return ptr::null_mut();
    };
    let Ok(agent_pk) = buzztalk_buzz::PublicKey::parse(&agent) else {
        return ptr::null_mut();
    };

    let buzz_config = BuzzConfig::new(
        relay,
        channel_id,
        KeySource::File(std::path::PathBuf::from(key_path)),
    )
    .with_agent_pubkeys(vec![agent_pk]);

    let Ok(agent_backend) = BuzzAgent::connect(buzz_config) else {
        return ptr::null_mut();
    };

    let config = PipelineConfig {
        agent: Box::new(agent_backend),
        // iOS goes VoiceProcessingIO-only — the two-stream fallback starves
        // a Bluetooth mic (see the port doc).
        use_voice_processing: true,
        ..Default::default()
    };

    match ConversationPipeline::start(config) {
        Ok(pipeline) => {
            pipeline.start_session();
            Box::into_raw(Box::new(BuzztalkSession { pipeline }))
        }
        Err(_) => ptr::null_mut(),
    }
}

/// Poll for one pipeline event. Writes the event kind into `out_kind` and,
/// when there is associated text, sets `*out_text` to a newly-allocated
/// C string the caller must free with [`buzztalk_string_free`] (it is left
/// null when there is no text). Returns 0 on success, -1 on a null handle.
#[no_mangle]
pub unsafe extern "C" fn buzztalk_poll_event(
    session: *mut BuzztalkSession,
    out_kind: *mut c_int,
    out_text: *mut *mut c_char,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    if !out_text.is_null() {
        *out_text = ptr::null_mut();
    }

    let (kind, text) = match session
        .pipeline
        .recv_event_timeout(std::time::Duration::from_millis(0))
    {
        Some(PipelineEvent::Partial(t)) => (BuzztalkEventKind::Partial, Some(t)),
        Some(PipelineEvent::FinalTranscript(t)) => (BuzztalkEventKind::FinalTranscript, Some(t)),
        Some(PipelineEvent::AgentText(t)) => (BuzztalkEventKind::AgentText, Some(t)),
        Some(PipelineEvent::StateChanged(s)) => {
            (BuzztalkEventKind::StateChanged, Some(format!("{s:?}")))
        }
        Some(PipelineEvent::AudioDeviceRebuilt { reason }) => {
            (BuzztalkEventKind::AudioRebuilt, Some(reason))
        }
        Some(PipelineEvent::CapabilityLost { what, reason }) => (
            BuzztalkEventKind::CapabilityLost,
            Some(format!("{what}: {reason}")),
        ),
        // Events the app doesn't need surfaced (capabilities, metrics,
        // drops) collapse to None so the Dart side has fewer cases.
        _ => (BuzztalkEventKind::None, None),
    };

    if !out_kind.is_null() {
        *out_kind = kind as c_int;
    }
    if let (Some(text), false) = (text, out_text.is_null()) {
        if let Ok(cstring) = CString::new(text) {
            *out_text = cstring.into_raw();
        }
    }
    0
}

/// Free a string previously handed out by [`buzztalk_poll_event`].
#[no_mangle]
pub unsafe extern "C" fn buzztalk_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// End the session and free the handle. The pointer is invalid afterwards.
#[no_mangle]
pub unsafe extern "C" fn buzztalk_stop(session: *mut BuzztalkSession) {
    if !session.is_null() {
        let session = Box::from_raw(session);
        session.pipeline.end_session();
        // `pipeline`'s Drop joins the orchestration + worker threads.
    }
}

/// Borrow a C string as an owned `String`, or `None` if null / not UTF-8.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_owned)
}
