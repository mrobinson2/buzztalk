//! Acoustic echo cancellation backends for BuzzTalk -- and the bake-off that
//! picked one.
//!
//! # The bake-off
//!
//! Three real AEC crates were evaluated by actually adding them as
//! dependencies and building, one at a time, behind their own Cargo feature.
//! `webrtc-audio-processing` was re-evaluated after `pkg-config`, `meson`,
//! and `ninja` became available on the build machine (it originally failed
//! for lacking them); `cmake` remains absent throughout but turned out not
//! to be needed by this candidate's `bundled` build path.
//!
//! | crate | builds here? | why |
//! |---|---|---|
//! | `webrtc-audio-processing` | yes, via `bundled` | Rust bindings to WebRTC's actual C++ Audio Processing Module. The dynamic-link default still needs a prebuilt system library; the `bundled` feature instead builds WebRTC's C++ from source with **meson + ninja** (not cmake -- this candidate never needed it). A clean build of just this dependency took **~28 s** wall clock on this 8-core Apple Silicon Mac (measured via `cargo clean -p webrtc-audio-processing-sys -p webrtc-audio-processing` followed by a timed rebuild) -- fast here, but meson/ninja is still a C++ toolchain every contributor and all three CI platforms (macOS/Linux/Windows) have to have installed and working, unlike the two candidates below. |
//! | `aec3` | yes, with a catch | pure-Rust port of WebRTC's AEC3, no system dependencies -- but its `LinearPipeline` holds an `Rc`-based graph runtime and is therefore **not `Send`**, which this crate's `EchoCanceller: Send` bound requires. Worked around here by confining the pipeline to a dedicated worker thread and talking to it over `std::sync::mpsc` (see `backend_aec3.rs`); that thread-hop is real integration cost this candidate carries that `sonora` and `webrtc-audio-processing` do not (both are natively `Send`). |
//! | `sonora` | yes | pure-Rust WebRTC APM port (AEC3 + NS + AGC2 + HPF), no system dependencies, and its `AudioProcessing` is natively `Send` -- no wrapper needed. |
//!
//! All three candidates pass the synthetic echo-reduction test in this crate
//! (`tests/echo_cancellation.rs`), converged over a 6-second synthetic call
//! with a 40 ms / -12 dB echo path, measured on the identical signal:
//!
//! | backend | echo reduction |
//! |---|---|
//! | `sonora` | **~20.0 dB** |
//! | `aec3` | **~15.6 dB** |
//! | `webrtc` (`webrtc-audio-processing`, the real WebRTC C++ APM) | **~15.4 dB** |
//! | `null` (control) | 0.0 dB |
//!
//! `sonora`'s edge over the real WebRTC APM on *this specific synthetic
//! signal* (a simple delay-plus-attenuation echo path, no reverberation or
//! nonlinearity) was not the expected outcome going in -- `webrtc-audio-
//! processing` is the reference implementation every pure-Rust port here is
//! trying to reproduce, and would very plausibly pull ahead on real-world
//! audio, double-talk, and edge cases this synthetic test doesn't exercise.
//! Take the ranking as evidence for *this* test, not a general verdict. Both
//! numbers depend on far-end signal choice -- broadband noise, not a
//! narrowband chirp; see that test's module docs for why a chirp made every
//! candidate look like it wasn't working at all.
//!
//! **Current default feature: `sonora`**, unchanged by the webrtc
//! re-evaluation pending a decision on whether the `bundled` C++/meson build
//! cost (paid by every contributor and all three CI platforms) is worth
//! taking on for a candidate that, on this test, didn't outperform the
//! pure-Rust options. See `Cargo.toml` for the `backend-webrtc` feature.
//!
//! [`available_backends`] reports which backends were compiled into *this*
//! build, and [`new_best_available`] prefers a real backend over
//! [`NullCanceller`] -- `webrtc`, then `sonora`, then `aec3` -- when more
//! than one is compiled into the same build (the shipped default only ever
//! compiles in one).
//!
//! **If no real backend is compiled in** (i.e. building with
//! `--no-default-features` and no `backend-*` feature re-enabled), every
//! canceller obtained from this crate is [`NullCanceller`] -- audio passes
//! through **completely unprocessed**. Check [`available_backends`] at
//! startup and surface it if the list only contains `"null"`.

use buzztalk_core::{AecStats, EchoCanceller, Error, Result};

#[cfg(feature = "backend-aec3")]
mod backend_aec3;
#[cfg(feature = "backend-sonora")]
mod backend_sonora;
#[cfg(feature = "backend-webrtc")]
mod backend_webrtc;

#[cfg(feature = "backend-aec3")]
pub use backend_aec3::Aec3Canceller;
#[cfg(feature = "backend-sonora")]
pub use backend_sonora::SonoraCanceller;
#[cfg(feature = "backend-webrtc")]
pub use backend_webrtc::WebrtcCanceller;

/// Passes capture audio through untouched and reports [`AecStats::default`].
///
/// This is both the always-available fallback (used whenever no real backend
/// was compiled in, or a real backend fails to initialise) and the control
/// condition for the bake-off: any backend that does not measurably
/// outperform this on the synthetic echo test is not worth shipping.
#[derive(Debug, Default)]
pub struct NullCanceller;

impl EchoCanceller for NullCanceller {
    fn process_render(&mut self, _far_end: &[f32]) -> Result<()> {
        Ok(())
    }

    fn process_capture(&mut self, _near_end: &mut [f32]) -> Result<()> {
        Ok(())
    }

    fn set_stream_delay_ms(&mut self, _delay_ms: u32) {}

    fn stats(&self) -> AecStats {
        AecStats::default()
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

/// Names of the backends compiled into this build, in preference order.
///
/// `"null"` is always last since it is the fallback, not a candidate.
#[allow(clippy::vec_init_then_push)] // pushes are individually cfg-gated
pub fn available_backends() -> Vec<&'static str> {
    let mut backends = Vec::new();
    #[cfg(feature = "backend-webrtc")]
    backends.push("webrtc");
    #[cfg(feature = "backend-sonora")]
    backends.push("sonora");
    #[cfg(feature = "backend-aec3")]
    backends.push("aec3");
    backends.push("null");
    backends
}

/// Constructs a canceller by backend name.
///
/// # Errors
///
/// Returns [`Error::Aec`] if `name` does not match a backend compiled into
/// this build.
pub fn new_backend(name: &str) -> Result<Box<dyn EchoCanceller>> {
    match name {
        "null" => Ok(Box::new(NullCanceller)),
        #[cfg(feature = "backend-aec3")]
        "aec3" => Ok(Box::new(Aec3Canceller::new()?)),
        #[cfg(feature = "backend-sonora")]
        "sonora" => Ok(Box::new(SonoraCanceller::new()?)),
        #[cfg(feature = "backend-webrtc")]
        "webrtc" => Ok(Box::new(WebrtcCanceller::new()?)),
        other => Err(Error::Aec(format!(
            "unknown or not-compiled-in AEC backend: {other:?} (available: {:?})",
            available_backends()
        ))),
    }
}

/// Constructs the best backend compiled into this build.
///
/// Prefers a real canceller over [`NullCanceller`]. Among real backends,
/// prefers `webrtc` (the reference WebRTC C++ APM), then `sonora`, then
/// `aec3` (see the crate-level docs for the measured basis of that
/// preference), falling back to `null` if no real backend was compiled in.
/// This ordering only matters when more than one `backend-*` feature is
/// enabled at once -- the shipped default enables exactly one.
pub fn new_best_available() -> Box<dyn EchoCanceller> {
    for name in available_backends() {
        if let Ok(canceller) = new_backend(name) {
            return canceller;
        }
    }
    // Unreachable in practice: `new_backend("null")` cannot fail.
    Box::new(NullCanceller)
}
