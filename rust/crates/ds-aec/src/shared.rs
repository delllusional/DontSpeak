//! Capture-side AEC pieces shared by Windows (WASAPI Communications APO) and Linux
//! (Pulse `module-echo-cancel`). Both feed a bounded `Mutex<VecDeque<f32>>` that
//! [`CaptureHandle`] drains while rodio renders. macOS uses lock-free ringbuf instead.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Send+Sync drain handle for an echo-cancelled capture buffer.
#[derive(Clone)]
pub struct CaptureHandle {
    cap: Arc<Mutex<VecDeque<f32>>>,
    rate: u32,
}

impl CaptureHandle {
    pub fn new(cap: Arc<Mutex<VecDeque<f32>>>, rate: u32) -> Self {
        Self { cap, rate }
    }

    /// Negotiated capture rate (feed through rate→16 kHz resampler for Parakeet).
    pub fn capture_rate(&self) -> u32 {
        self.rate
    }

    /// Echo-cancelled mono f32 since last call.
    pub fn drain(&self) -> Vec<f32> {
        let mut q = self.cap.lock().unwrap();
        q.drain(..).collect()
    }
}

/// Render-side no-op for capture-side backends (`owns_render() == false`; rodio renders).
/// Keeps the helper's duplex feeder cfg-free on every OS.
#[derive(Clone)]
pub struct RenderHandle {
    _private: (),
}

impl RenderHandle {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }

    pub fn push(&self, _pcm_24k: &[f32]) {}

    pub fn buffered(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    /// Mute is macOS-only (VPIO owns render); here rodio volume mutes.
    pub fn set_muted(&self, _on: bool) {}
}

/// Append samples; drop oldest if over `cap_limit` (stalled listen must not grow unbounded).
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
        // Stalled listen: pre-existing samples + new chunk → drop oldest, keep newest.
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
