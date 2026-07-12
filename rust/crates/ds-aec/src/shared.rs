//! Capture-side AEC pieces shared by the Windows (WASAPI Communications APO) and Linux
//! (PulseAudio/PipeWire `module-echo-cancel`) backends. Both feed a `Mutex<VecDeque<f32>>`
//! bounded ring that a [`CaptureHandle`] drains while rodio renders TTS. macOS instead uses
//! a lock-free `ringbuf` SPSC, so it keeps its own `CaptureHandle` and overflow handling.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A `Send`+`Sync` drain handle for an echo-cancelled capture buffer (see
/// `DuplexAudio::capture_handle`). Identical for the Windows and Linux backends.
#[derive(Clone)]
pub struct CaptureHandle {
    cap: Arc<Mutex<VecDeque<f32>>>,
    rate: u32,
}

impl CaptureHandle {
    /// Build a handle over a backend's shared capture buffer + its negotiated rate.
    pub fn new(cap: Arc<Mutex<VecDeque<f32>>>, rate: u32) -> Self {
        Self { cap, rate }
    }

    /// The negotiated capture sample rate (feed through a `rate`→16 kHz resampler).
    pub fn capture_rate(&self) -> u32 {
        self.rate
    }

    /// Drain the echo-cancelled mono f32 captured since the last call.
    pub fn drain(&self) -> Vec<f32> {
        let mut q = self.cap.lock().unwrap();
        q.drain(..).collect()
    }
}

/// Append `samples` to the shared capture buffer, dropping the oldest f32 once it grows past
/// `cap_limit` — a stalled listen must never grow the ring without bound. The single
/// overflow-trim rule for both capture threads.
pub fn enqueue_bounded(cap: &Arc<Mutex<VecDeque<f32>>>, samples: &[f32], cap_limit: usize) {
    let mut q = cap.lock().unwrap();
    q.extend(samples.iter().copied());
    if q.len() > cap_limit {
        let drop = q.len() - cap_limit;
        q.drain(..drop);
    }
}

#[cfg(test)]
mod enqueue_bounded_tests {
    use super::*;

    fn ring(initial: &[f32]) -> Arc<Mutex<VecDeque<f32>>> {
        Arc::new(Mutex::new(VecDeque::from(initial.to_vec())))
    }

    fn contents(cap: &Arc<Mutex<VecDeque<f32>>>) -> Vec<f32> {
        cap.lock().unwrap().iter().copied().collect()
    }

    #[test]
    fn under_the_bound_keeps_everything_in_order() {
        let cap = ring(&[]);
        enqueue_bounded(&cap, &[1.0, 2.0, 3.0], 10);
        assert_eq!(contents(&cap), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn exactly_at_the_bound_keeps_everything() {
        let cap = ring(&[]);
        enqueue_bounded(&cap, &[1.0, 2.0, 3.0], 3);
        assert_eq!(contents(&cap), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn one_over_the_bound_drops_the_single_oldest_sample() {
        let cap = ring(&[]);
        enqueue_bounded(&cap, &[1.0, 2.0, 3.0, 4.0], 3);
        // Oldest (1.0) is dropped; the newest 3 survive, order preserved.
        assert_eq!(contents(&cap), vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn far_over_the_bound_drops_all_the_excess_oldest_samples_at_once() {
        let cap = ring(&[]);
        let batch: Vec<f32> = (0..10).map(|i| i as f32).collect();
        enqueue_bounded(&cap, &batch, 4);
        assert_eq!(contents(&cap), vec![6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn preexisting_samples_plus_a_new_chunk_drops_the_oldest_first() {
        // Simulates a stalled listen: the ring already holds samples from earlier
        // pushes, and a new chunk tips it over `cap_limit` — the OLDEST
        // (pre-existing) samples must be the ones dropped, never the newest.
        let cap = ring(&[1.0, 2.0]);
        enqueue_bounded(&cap, &[3.0, 4.0, 5.0], 3);
        assert_eq!(contents(&cap), vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn repeated_small_enqueues_stay_bounded_and_keep_only_the_most_recent() {
        let cap = ring(&[]);
        for i in 0..100 {
            enqueue_bounded(&cap, &[i as f32], 5);
        }
        assert_eq!(contents(&cap), vec![95.0, 96.0, 97.0, 98.0, 99.0]);
    }

    #[test]
    fn cap_limit_zero_drops_everything_pushed() {
        let cap = ring(&[]);
        enqueue_bounded(&cap, &[1.0, 2.0, 3.0], 0);
        assert!(contents(&cap).is_empty());
    }

    #[test]
    fn empty_input_is_a_no_op() {
        let cap = ring(&[1.0, 2.0]);
        enqueue_bounded(&cap, &[], 10);
        assert_eq!(contents(&cap), vec![1.0, 2.0]);
    }
}
