//! A fixed-duration ring buffer of recent audio, replayed on confirmed
//! barge-in.

use std::collections::VecDeque;

use crate::types::{Frame, PREROLL_FRAMES};

/// Holds the last [`crate::PREROLL_DURATION`] of audio frames.
///
/// Filled continuously while a session is active (via
/// [`crate::SessionMachine::push_frame`]) so that when a barge-in is
/// confirmed, the ~40 ms of detector-confirmation latency doesn't cost the
/// first word of what the user said -- it was already captured here and can
/// be replayed ahead of live audio.
#[derive(Debug, Clone)]
pub struct PreRollBuffer {
    frames: VecDeque<Frame>,
    capacity: usize,
}

impl PreRollBuffer {
    /// A buffer sized to [`crate::PREROLL_DURATION`].
    pub fn new() -> Self {
        Self::with_capacity(PREROLL_FRAMES)
    }

    /// A buffer holding exactly `capacity` frames (0 is treated as 1, since
    /// a zero-capacity ring buffer can't hold anything to replay).
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push the newest frame, evicting the oldest one if full.
    pub fn push(&mut self, frame: Frame) {
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    /// Number of frames currently held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the buffer holds no frames yet.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Maximum number of frames this buffer will hold.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drop all buffered frames.
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// A snapshot of the buffered frames, oldest first -- the order they
    /// should be replayed in.
    pub fn frames_oldest_first(&self) -> Vec<Frame> {
        self.frames.iter().copied().collect()
    }
}

impl Default for PreRollBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(v: f32) -> Frame {
        let mut f = [0.0_f32; buzztalk_core::FRAME_SAMPLES];
        f[0] = v;
        f
    }

    #[test]
    fn empty_buffer_yields_nothing() {
        let buf = PreRollBuffer::with_capacity(4);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.frames_oldest_first().is_empty());
    }

    #[test]
    fn under_capacity_preserves_push_order() {
        let mut buf = PreRollBuffer::with_capacity(4);
        buf.push(marker(1.0));
        buf.push(marker(2.0));
        buf.push(marker(3.0));
        let got: Vec<f32> = buf.frames_oldest_first().iter().map(|f| f[0]).collect();
        assert_eq!(got, vec![1.0, 2.0, 3.0]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn wraps_and_keeps_only_the_newest_capacity_frames_in_order() {
        let mut buf = PreRollBuffer::with_capacity(4);
        for i in 1..=10 {
            buf.push(marker(i as f32));
        }
        assert_eq!(buf.len(), 4);
        let got: Vec<f32> = buf.frames_oldest_first().iter().map(|f| f[0]).collect();
        // Frames 1..=6 have been evicted; only the newest 4 remain, oldest first.
        assert_eq!(got, vec![7.0, 8.0, 9.0, 10.0]);
    }

    #[test]
    fn clear_empties_the_buffer() {
        let mut buf = PreRollBuffer::with_capacity(4);
        buf.push(marker(1.0));
        buf.push(marker(2.0));
        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.frames_oldest_first().is_empty());
    }

    #[test]
    fn default_capacity_matches_500ms_at_10ms_frames() {
        let buf = PreRollBuffer::new();
        assert_eq!(buf.capacity(), 50);
    }

    #[test]
    fn zero_requested_capacity_is_clamped_to_one() {
        let mut buf = PreRollBuffer::with_capacity(0);
        assert_eq!(buf.capacity(), 1);
        buf.push(marker(1.0));
        buf.push(marker(2.0));
        assert_eq!(buf.frames_oldest_first(), vec![marker(2.0)]);
    }
}
