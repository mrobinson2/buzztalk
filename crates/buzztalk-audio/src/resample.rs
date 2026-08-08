//! A minimal streaming linear resampler, used only when a device refuses to
//! open at [`buzztalk_core::SAMPLE_RATE_HZ`].
//!
//! This is deliberately simple: linear interpolation with **no
//! anti-aliasing filter**. That is a real quality trade-off for large rate
//! changes, but for the deltas real audio hardware actually exposes
//! (44.1 kHz <-> 48 kHz, 48 kHz <-> 96 kHz) it is inaudible for speech, and
//! critically it holds no state beyond a handful of scalars and never
//! allocates, so it is safe to drive from inside a real-time audio
//! callback. If BuzzTalk ever needs to resample telephony-grade or
//! music-grade audio through a large ratio, replace this with a proper
//! windowed-sinc resampler and keep the same push/pull interface.

/// Streaming linear resampler between two fixed sample rates.
///
/// Two complementary drive modes share the same interpolation state:
///
/// * [`Resampler::push`] — for a *producer-driven* source (a capture
///   callback handing us samples as they arrive): feed one input sample at
///   a time, get zero or more output samples via a callback.
/// * [`Resampler::pull`] — for a *consumer-driven* sink (an output callback
///   that needs exactly one sample right now): ask for one output sample,
///   the resampler calls back into `next_input` as many times as it needs
///   to advance the input stream.
///
/// Neither method allocates.
#[derive(Debug, Clone, Copy)]
pub struct Resampler {
    /// How far the read position advances through the input per output
    /// sample, in units of "one input sample interval". Equal to
    /// `from_hz / to_hz`.
    step: f64,
    /// Position of the next output sample within the current input
    /// interval `[prev, cur)`, in the same units as `step`.
    frac: f64,
    prev: f32,
    cur: f32,
    primed: bool,
}

impl Resampler {
    /// Build a resampler converting a stream at `from_hz` to `to_hz`.
    ///
    /// Both rates must be non-zero. Equal rates are allowed and degenerate
    /// to pass-through, but callers on a hot path should prefer to skip the
    /// resampler entirely when rates match (see how `DuplexEngine` only
    /// constructs one when the negotiated device rate differs from
    /// [`buzztalk_core::SAMPLE_RATE_HZ`]).
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        debug_assert!(from_hz > 0 && to_hz > 0, "resampler rates must be non-zero");
        Self {
            step: from_hz as f64 / to_hz as f64,
            frac: 0.0,
            prev: 0.0,
            cur: 0.0,
            primed: false,
        }
    }

    /// Push one `from_hz`-rate sample. Calls `emit` once for every
    /// `to_hz`-rate sample this input completes. Never allocates.
    pub fn push(&mut self, sample: f32, mut emit: impl FnMut(f32)) {
        if !self.primed {
            // Prime both taps with the first sample so the first output
            // isn't interpolated against silence.
            self.prev = sample;
            self.cur = sample;
            self.primed = true;
            return;
        }
        self.prev = self.cur;
        self.cur = sample;
        while self.frac < 1.0 {
            let t = self.frac as f32;
            emit(self.prev + (self.cur - self.prev) * t);
            self.frac += self.step;
        }
        self.frac -= 1.0;
    }

    /// Produce exactly one `to_hz`-rate output sample, pulling as many
    /// `from_hz`-rate input samples as needed via `next_input`. Never
    /// allocates.
    pub fn pull(&mut self, mut next_input: impl FnMut() -> f32) -> f32 {
        if !self.primed {
            let s = next_input();
            self.prev = s;
            self.cur = s;
            self.primed = true;
        }
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            self.prev = self.cur;
            self.cur = next_input();
        }
        let t = self.frac as f32;
        let out = self.prev + (self.cur - self.prev) * t;
        self.frac += self.step;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rate_delays_by_exactly_one_sample() {
        // At a 1:1 rate the algorithm is exact (no interpolation error),
        // but it is causal: an output can only be emitted once the *next*
        // input sample has arrived to confirm the current one isn't being
        // interpolated against the future. So every push after the first
        // (priming) one emits the *previous* sample, not the one just
        // pushed — a fixed one-sample latency, not data loss or reordering.
        let mut r = Resampler::new(48_000, 48_000);
        let mut out = Vec::new();
        for s in [0.1_f32, 0.2, 0.3, 0.4] {
            r.push(s, |v| out.push(v));
        }
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn downsampling_emits_fewer_outputs_than_inputs() {
        // 96kHz -> 48kHz should emit roughly one output per two inputs.
        let mut r = Resampler::new(96_000, 48_000);
        let mut out_count = 0usize;
        for i in 0..1000 {
            r.push(i as f32, |_| out_count += 1);
        }
        assert!(
            (400..600).contains(&out_count),
            "expected ~500 outputs for 1000 inputs at 2:1, got {out_count}"
        );
    }

    #[test]
    fn upsampling_emits_more_outputs_than_inputs() {
        // 48kHz -> 96kHz should emit roughly two outputs per input.
        let mut r = Resampler::new(48_000, 96_000);
        let mut out_count = 0usize;
        for i in 0..1000 {
            r.push(i as f32, |_| out_count += 1);
        }
        assert!(
            (1900..2100).contains(&out_count),
            "expected ~2000 outputs for 1000 inputs at 1:2, got {out_count}"
        );
    }

    #[test]
    fn pull_matches_push_long_run_rate() {
        // Feed a ramp into the input queue and pull device-rate samples out;
        // the number of inputs consumed should track from_hz/to_hz.
        let mut r = Resampler::new(48_000, 44_100);
        let mut input = (0..2000).map(|i| i as f32);
        let mut consumed = 0usize;
        let mut produced = 0usize;
        for _ in 0..1000 {
            let _ = r.pull(|| {
                consumed += 1;
                input.next().unwrap_or(0.0)
            });
            produced += 1;
        }
        // 1000 outputs at 48000/44100 ratio should consume ~1088 inputs.
        assert!(
            (1000..1200).contains(&consumed),
            "consumed {consumed} inputs for {produced} outputs"
        );
    }

    #[test]
    fn never_panics_on_constant_input() {
        let mut r = Resampler::new(44_100, 48_000);
        for _ in 0..10_000 {
            r.push(0.5, |v| assert!(v.is_finite()));
        }
    }
}
