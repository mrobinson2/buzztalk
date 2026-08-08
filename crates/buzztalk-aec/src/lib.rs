//! Acoustic echo cancellation backends for BuzzTalk -- and the bake-off that
//! picked one.
//!
//! # The bake-off
//!
//! Three real AEC crates were evaluated by actually adding them as
//! dependencies and building, one at a time, behind their own Cargo feature:
//!
//! | crate | builds here? | why |
//! |---|---|---|
//! | `webrtc-audio-processing` | **no** | C++ WebRTC APM bindings. The default (dynamic-link) path fails because `pkg-config` itself is not installed. The `bundled` feature (build WebRTC's C++ from source) fails because `meson` is not installed. Neither `cmake`, `meson`, nor `ninja` are present in this environment. |
//! | `aec3` | yes, with a catch | pure-Rust port of WebRTC's AEC3, no system dependencies -- but its `LinearPipeline` holds an `Rc`-based graph runtime and is therefore **not `Send`**, which this crate's `EchoCanceller: Send` bound requires. Worked around here by confining the pipeline to a dedicated worker thread and talking to it over `std::sync::mpsc` (see `backend_aec3.rs`); that thread-hop is real integration cost this candidate carries that `sonora` does not. |
//! | `sonora` | yes | pure-Rust WebRTC APM port (AEC3 + NS + AGC2 + HPF), no system dependencies, and its `AudioProcessing` is natively `Send` -- no wrapper needed. |
//!
//! Both pure-Rust candidates pass the synthetic echo-reduction test in this
//! crate (`tests/echo_cancellation.rs`), converged over a 6-second synthetic
//! call with a 40 ms / -12 dB echo path: `aec3` reduced echo energy by
//! **~15.6 dB**, `sonora` by **~20.0 dB**. Both numbers depend on far-end
//! signal choice -- broadband noise, not a narrowband chirp; see that test's
//! module docs for why a chirp made both candidates look like they weren't
//! working at all.
//!
//! **Recommendation: `sonora`**, and it is this crate's default feature. It
//! measured a larger reduction, and it does not need the `aec3` backend's
//! thread-confinement workaround. If `cmake`/`meson`/`ninja`/`pkg-config`
//! become available, `webrtc-audio-processing` (the actual, battle-tested
//! WebRTC C++ APM, not a from-scratch Rust port) would be worth re-evaluating
//! -- it is the implementation every other candidate here is trying to
//! reproduce.
//!
//! [`available_backends`] reports which backends were compiled into *this*
//! build, and [`new_best_available`] prefers a real backend (`sonora`, then
//! `aec3`) over [`NullCanceller`].
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

#[cfg(feature = "backend-aec3")]
pub use backend_aec3::Aec3Canceller;
#[cfg(feature = "backend-sonora")]
pub use backend_sonora::SonoraCanceller;

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
        other => Err(Error::Aec(format!(
            "unknown or not-compiled-in AEC backend: {other:?} (available: {:?})",
            available_backends()
        ))),
    }
}

/// Constructs the best backend compiled into this build.
///
/// Prefers a real canceller over [`NullCanceller`]. Among real backends,
/// prefers `sonora` (see the crate-level docs for the measured basis of that
/// preference), falling back to `aec3`, falling back to `null` if neither
/// real backend was compiled in.
pub fn new_best_available() -> Box<dyn EchoCanceller> {
    for name in available_backends() {
        if let Ok(canceller) = new_backend(name) {
            return canceller;
        }
    }
    // Unreachable in practice: `new_backend("null")` cannot fail.
    Box::new(NullCanceller)
}
