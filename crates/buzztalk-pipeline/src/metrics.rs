//! Per-turn latency instrumentation.
//!
//! Latency is the product: a barge-in that takes half a second to actually
//! silence the speaker is not a working interruption, and a "final
//! transcript" that arrives seconds after the user stopped talking makes
//! the agent feel like it isn't listening. Both numbers below are measured,
//! not estimated, and are reported (see
//! [`crate::PipelineEvent::TurnMetrics`]) at the end of every turn.

use std::time::Instant;

/// Latency measurements for one conversational turn.
#[derive(Debug, Default, Clone, Copy)]
pub struct TurnMetrics {
    endpoint_at: Option<Instant>,
    final_transcript_ms: Option<f64>,
    barge_in_at: Option<Instant>,
    barge_in_silence_ms: Option<f64>,
}

impl TurnMetrics {
    /// A fresh, empty set of measurements for a new turn.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the instant user speech ended (the endpoint detector fired).
    pub fn mark_endpoint(&mut self) {
        self.endpoint_at = Some(Instant::now());
    }

    /// Mark that a final transcript was just accepted; records the elapsed
    /// time since [`Self::mark_endpoint`], if that was called this turn.
    pub fn mark_final_transcript(&mut self) {
        if let Some(t0) = self.endpoint_at {
            self.final_transcript_ms = Some(ms_since(t0));
        }
    }

    /// Mark the instant a barge-in was detected and playback was told to
    /// cancel.
    pub fn mark_barge_in(&mut self) {
        self.barge_in_at = Some(Instant::now());
    }

    /// Mark that playback has been observed to actually go silent (the
    /// engine reported a fresh underrun with nothing left queued); records
    /// the elapsed time since [`Self::mark_barge_in`]. A no-op if that
    /// wasn't called, or if this has already been recorded for the current
    /// barge-in.
    pub fn mark_playback_silent(&mut self) {
        if let (Some(t0), None) = (self.barge_in_at, self.barge_in_silence_ms) {
            self.barge_in_silence_ms = Some(ms_since(t0));
        }
    }

    /// End-of-speech to final-transcript latency, in milliseconds, if both
    /// ends of that measurement happened this turn.
    pub fn final_transcript_ms(&self) -> Option<f64> {
        self.final_transcript_ms
    }

    /// Barge-in-detected to playback-actually-silent latency, in
    /// milliseconds, if a barge-in was both detected and resolved this
    /// turn.
    pub fn barge_in_silence_ms(&self) -> Option<f64> {
        self.barge_in_silence_ms
    }

    /// Whether either measurement was captured this turn -- i.e. whether
    /// [`Self::summary`] has anything worth printing.
    pub fn has_any(&self) -> bool {
        self.final_transcript_ms.is_some() || self.barge_in_silence_ms.is_some()
    }

    /// One-line human-readable summary for end-of-turn logging.
    pub fn summary(&self) -> String {
        let final_txt = fmt_ms(self.final_transcript_ms);
        let barge = fmt_ms(self.barge_in_silence_ms);
        format!(
            "end-of-speech -> final transcript: {final_txt}   barge-in -> playback silent: {barge}"
        )
    }
}

fn ms_since(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

fn fmt_ms(v: Option<f64>) -> String {
    match v {
        Some(ms) => format!("{ms:.1} ms"),
        None => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn fresh_metrics_have_nothing_to_report() {
        let m = TurnMetrics::new();
        assert!(!m.has_any());
        assert_eq!(m.final_transcript_ms(), None);
        assert_eq!(m.barge_in_silence_ms(), None);
        assert!(m.summary().contains("n/a"));
    }

    #[test]
    fn final_transcript_latency_is_measured_relative_to_endpoint() {
        let mut m = TurnMetrics::new();
        m.mark_endpoint();
        sleep(Duration::from_millis(5));
        m.mark_final_transcript();
        let ms = m.final_transcript_ms().expect("should be recorded");
        assert!(ms >= 4.0, "expected >=4ms elapsed, got {ms}");
        assert!(m.has_any());
    }

    #[test]
    fn final_transcript_without_endpoint_is_not_recorded() {
        let mut m = TurnMetrics::new();
        m.mark_final_transcript();
        assert_eq!(m.final_transcript_ms(), None);
    }

    #[test]
    fn barge_in_silence_latency_is_measured_relative_to_barge_in() {
        let mut m = TurnMetrics::new();
        m.mark_barge_in();
        sleep(Duration::from_millis(5));
        m.mark_playback_silent();
        let ms = m.barge_in_silence_ms().expect("should be recorded");
        assert!(ms >= 4.0, "expected >=4ms elapsed, got {ms}");
    }

    #[test]
    fn mark_playback_silent_only_records_once_per_barge_in() {
        let mut m = TurnMetrics::new();
        m.mark_barge_in();
        sleep(Duration::from_millis(5));
        m.mark_playback_silent();
        let first = m.barge_in_silence_ms().unwrap();
        sleep(Duration::from_millis(5));
        m.mark_playback_silent();
        assert_eq!(
            m.barge_in_silence_ms(),
            Some(first),
            "second call should be a no-op"
        );
    }

    #[test]
    fn both_measurements_can_coexist_in_one_turn() {
        let mut m = TurnMetrics::new();
        m.mark_endpoint();
        m.mark_final_transcript();
        m.mark_barge_in();
        m.mark_playback_silent();
        assert!(m.final_transcript_ms().is_some());
        assert!(m.barge_in_silence_ms().is_some());
        let summary = m.summary();
        assert!(!summary.contains("n/a"));
    }
}
