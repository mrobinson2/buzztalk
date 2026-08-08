//! 48 kHz -> 16 kHz resampling.
//!
//! BuzzTalk's pipeline runs at [`buzztalk_core::SAMPLE_RATE_HZ`] (48 kHz);
//! the STT model wants 16 kHz. The ratio is an exact 3:1, so this is a
//! decimation problem, not general-purpose rate conversion: a causal
//! windowed-sinc low-pass filter (cutoff at the 8 kHz output Nyquist) is
//! applied continuously across calls, and every third filtered sample is
//! kept.
//!
//! State (the filter's sample history and the decimation phase) persists
//! across [`Resampler48to16::process`] calls, so feeding audio in arbitrary
//! chunk sizes gives the same output as feeding it in one call — chunking is
//! purely a caller-side convenience, not something that should change the
//! result.

use std::collections::VecDeque;

/// Input sample rate this resampler accepts.
const IN_HZ: u32 = 48_000;
/// Output sample rate this resampler produces.
const OUT_HZ: u32 = 16_000;
/// Exact input:output decimation ratio (48 kHz / 16 kHz).
const DECIMATION: u64 = (IN_HZ / OUT_HZ) as u64;
/// Number of taps in the causal low-pass FIR filter. Odd and modest: this is
/// a bandwidth-limiting filter ahead of an ASR model, not a mastering-grade
/// resampler.
const NUM_TAPS: usize = 31;

/// Resamples 48 kHz mono `f32` audio to 16 kHz mono `f32` audio.
///
/// Does not depend on `buzztalk-audio` or any other sibling crate — it is a
/// self-contained, pure-Rust decimator so `buzztalk-stt` can be exercised
/// without a full audio stack.
pub struct Resampler48to16 {
    taps: [f32; NUM_TAPS],
    /// Most recent `NUM_TAPS` input samples, oldest first. Pre-filled with
    /// zeros so the filter is well-defined from the very first input sample
    /// (at the cost of a short, silent warm-up region in the output).
    history: VecDeque<f32>,
    /// Count of raw input samples consumed so far; used to pick the
    /// decimation phase (`sample_index % DECIMATION == 0`) continuously
    /// across calls.
    sample_index: u64,
}

impl Resampler48to16 {
    /// Create a new resampler with a freshly zeroed filter history.
    pub fn new() -> Self {
        Self {
            taps: design_lowpass(),
            history: VecDeque::from(vec![0.0_f32; NUM_TAPS]),
            sample_index: 0,
        }
    }

    /// Resample a chunk of 48 kHz mono samples. The returned buffer holds
    /// roughly `input.len() / 3` samples at 16 kHz — exactly that many once
    /// enough audio has flowed through to clear the decimation phase.
    pub fn process(&mut self, input_48k: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(input_48k.len() / DECIMATION as usize + 1);
        for &sample in input_48k {
            self.history.pop_front();
            self.history.push_back(sample);

            if self.sample_index % DECIMATION == 0 {
                // history is oldest-first; taps[0] is the most recent tap.
                let y: f32 = self
                    .history
                    .iter()
                    .rev()
                    .zip(self.taps.iter())
                    .map(|(x, h)| x * h)
                    .sum();
                out.push(y);
            }
            self.sample_index += 1;
        }
        out
    }

    /// Reset filter history and decimation phase, e.g. after a discontinuity
    /// (device change, seek, or turn boundary).
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|s| *s = 0.0);
        self.sample_index = 0;
    }
}

impl Default for Resampler48to16 {
    fn default() -> Self {
        Self::new()
    }
}

/// Design a causal, Hamming-windowed-sinc low-pass FIR with cutoff at the
/// output Nyquist frequency (`OUT_HZ / 2`), normalized to unity DC gain.
fn design_lowpass() -> [f32; NUM_TAPS] {
    let fc = (OUT_HZ as f32 / 2.0) / IN_HZ as f32; // cutoff as a fraction of IN_HZ
    let m = (NUM_TAPS - 1) as f32;
    let mut taps = [0.0_f32; NUM_TAPS];

    for (n, tap) in taps.iter_mut().enumerate() {
        let k = n as f32 - m / 2.0;
        let sinc = if k == 0.0 {
            2.0 * fc
        } else {
            (2.0 * std::f32::consts::PI * fc * k).sin() / (std::f32::consts::PI * k)
        };
        let window = 0.54 - 0.46 * (2.0 * std::f32::consts::PI * n as f32 / m).cos();
        *tap = sinc * window;
    }

    let dc_gain: f32 = taps.iter().sum();
    for tap in taps.iter_mut() {
        *tap /= dc_gain;
    }
    taps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_ratio_of_input_to_output_length() {
        let mut r = Resampler48to16::new();
        let input = vec![0.0_f32; 48_000];
        let out = r.process(&input);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let input: Vec<f32> = (0..9000).map(|i| (i as f32 * 0.01).sin()).collect();

        let mut whole = Resampler48to16::new();
        let out_whole = whole.process(&input);

        let mut chunked = Resampler48to16::new();
        let mut out_chunked = Vec::new();
        for chunk in input.chunks(37) {
            out_chunked.extend(chunked.process(chunk));
        }

        assert_eq!(out_whole.len(), out_chunked.len());
        for (a, b) in out_whole.iter().zip(out_chunked.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn preserves_dc_after_warmup() {
        let mut r = Resampler48to16::new();
        // Feed enough constant signal that the filter history is entirely
        // past the zero-padded warm-up region.
        let input = vec![0.5_f32; 4 * NUM_TAPS * DECIMATION as usize];
        let out = r.process(&input);
        for &y in out.iter().skip(out.len() / 2) {
            assert!((y - 0.5).abs() < 1e-3, "expected ~0.5, got {y}");
        }
    }

    #[test]
    fn silence_in_silence_out() {
        let mut r = Resampler48to16::new();
        let out = r.process(&[0.0_f32; 4800]);
        assert!(out.iter().all(|&y| y == 0.0));
    }

    #[test]
    fn reset_clears_history_and_phase() {
        let mut r = Resampler48to16::new();
        let _ = r.process(&[1.0_f32; 100]);
        r.reset();
        // Right after reset, behavior must match a brand-new resampler fed
        // the same input.
        let mut fresh = Resampler48to16::new();
        let out_reset = r.process(&[0.25_f32; 300]);
        let out_fresh = fresh.process(&[0.25_f32; 300]);
        assert_eq!(out_reset, out_fresh);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let mut r = Resampler48to16::new();
        assert!(r.process(&[]).is_empty());
    }
}
