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
    while q.len() > cap_limit {
        let drop = q.len() - cap_limit;
        q.drain(..drop);
    }
}
