//! Chunking an arbitrary-length, already-48kHz sample stream into exact
//! [`FRAME_SAMPLES`]-length frames.
//!
//! This sits on the **consumer** side of a ring buffer, never inside an
//! audio callback: the callback side only ever does raw, single-sample
//! `try_push`/`try_pop` on the ring, which is what keeps it real-time safe.
//! Framing — and the small allocation each full frame requires to hand the
//! caller an owned `Vec<f32>` — happens here, off the audio thread.

use buzztalk_core::FRAME_SAMPLES;
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

/// Pulls raw f32 samples off a ring and hands back exact
/// [`FRAME_SAMPLES`]-length frames.
///
/// Only one `FrameReader` should ever drain a given ring (it is the
/// consumer half of an SPSC ring), but the reader itself is a plain owned
/// value: move it to whatever thread does the draining.
pub struct FrameReader {
    consumer: HeapCons<f32>,
    partial: Vec<f32>,
}

impl FrameReader {
    pub(crate) fn new(consumer: HeapCons<f32>) -> Self {
        Self {
            consumer,
            partial: Vec::with_capacity(FRAME_SAMPLES),
        }
    }

    /// Non-blocking. Returns `Some(frame)` once a full `FRAME_SAMPLES`
    /// frame is available; otherwise returns `None` and keeps whatever
    /// partial progress it made for the next call.
    pub fn try_recv(&mut self) -> Option<Vec<f32>> {
        while self.partial.len() < FRAME_SAMPLES {
            self.partial.push(self.consumer.try_pop()?);
        }
        Some(std::mem::replace(
            &mut self.partial,
            Vec::with_capacity(FRAME_SAMPLES),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::HeapRb;

    #[test]
    fn returns_none_until_a_full_frame_is_available() {
        let rb = HeapRb::<f32>::new(4096);
        let (mut prod, cons) = rb.split();
        let mut reader = FrameReader::new(cons);

        for _ in 0..FRAME_SAMPLES - 1 {
            prod.try_push(1.0).unwrap();
        }
        assert!(reader.try_recv().is_none());

        prod.try_push(1.0).unwrap();
        let frame = reader.try_recv().expect("frame should now be complete");
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().all(|&s| s == 1.0));
    }

    #[test]
    fn arbitrary_length_pushes_produce_exact_frames() {
        let rb = HeapRb::<f32>::new(FRAME_SAMPLES * 10);
        let (mut prod, cons) = rb.split();
        let mut reader = FrameReader::new(cons);

        // Push in odd-sized, non-frame-aligned chunks.
        let total = FRAME_SAMPLES * 3 + 17;
        let mut pushed = 0usize;
        for chunk_len in [7, 130, 480, 1, 999] {
            let n = chunk_len.min(total - pushed);
            for i in 0..n {
                prod.try_push((pushed + i) as f32).unwrap();
            }
            pushed += n;
            if pushed >= total {
                break;
            }
        }

        let mut frames = Vec::new();
        while let Some(f) = reader.try_recv() {
            frames.push(f);
        }
        assert_eq!(frames.len(), 3, "3 full frames out of {total} samples");
        for frame in &frames {
            assert_eq!(frame.len(), FRAME_SAMPLES);
        }
        // Leftover 17 samples remain buffered internally, not yet a frame.
        assert!(reader.try_recv().is_none());
    }

    #[test]
    fn partial_progress_survives_across_calls() {
        let rb = HeapRb::<f32>::new(FRAME_SAMPLES * 2);
        let (mut prod, cons) = rb.split();
        let mut reader = FrameReader::new(cons);

        prod.try_push(9.0).unwrap();
        assert!(reader.try_recv().is_none());
        prod.try_push(9.0).unwrap();
        assert!(reader.try_recv().is_none());

        for _ in 0..(FRAME_SAMPLES - 2) {
            prod.try_push(9.0).unwrap();
        }
        let frame = reader.try_recv().unwrap();
        assert_eq!(frame.len(), FRAME_SAMPLES);
    }

    #[test]
    fn silence_frames_round_trip_correctly() {
        let rb = HeapRb::<f32>::new(FRAME_SAMPLES * 2);
        let (mut prod, cons) = rb.split();
        let mut reader = FrameReader::new(cons);

        for _ in 0..FRAME_SAMPLES {
            prod.try_push(0.0).unwrap();
        }
        let frame = reader.try_recv().unwrap();
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().all(|&s| s == 0.0));
    }
}
