//! Linux PipeWire/PulseAudio `module-echo-cancel` duplex audio (capture-side AEC).
//!
//! Like Windows WASAPI (and unlike macOS VPIO, which owns BOTH render and capture on one
//! clock), the Linux native AEC is **capture-side**: the sound server runs the WebRTC
//! canceller in `module-echo-cancel` and exposes a **cancelled virtual source** that
//! references the system render endpoint *itself*. So we do NOT route TTS through this
//! unit — rodio keeps rendering normally ([`owns_render`](DuplexAudio::owns_render) is
//! `false`) and this backend only opens that echo-cancelled source.
//!
//! We open it through the **PulseAudio simple API**, which talks to PulseAudio AND PipeWire
//! (via `pipewire-pulse`), so one path covers both servers. The source is selected by name:
//! `$DONTSPEAK_AEC_SOURCE`, else our shipped config drop-in's `ds_ec_source`, else the
//! common `echo-cancel-source`. Any connect/format failure is fail-quiet — `open()` returns
//! `Err` and the caller degrades to the half-duplex cpal + rodio path.
//!
//! (A future deterministic alternative — linking the WebRTC APM in-process with a TTS render
//! tap, `owns_render() == true` — is sketched in docs/FULL-DUPLEX-PORT.md §6A; this
//! server-module path ships first as the recommended primary on modern distros.)
//!
//! Threading mirrors Windows: a dedicated thread owns the blocking `Simple` record stream,
//! reads ~20 ms chunks (so it re-checks `stop` promptly), and pushes echo-cancelled mono
//! f32 into a shared bounded buffer a [`CaptureHandle`] drains.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::shared::{CaptureHandle, enqueue_bounded};

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

/// Negotiated capture rate. We ask the server for 48 kHz mono f32 (it resamples the
/// already-cancelled source for us); the helper resamples 48 k → 16 k before Parakeet.
const CAPTURE_RATE: u32 = 48_000;
/// ~2 s cap: the helper drains every poll tick; if a listen stalls, drop OLDEST samples.
const CAPTURE_SECS: usize = 2;
/// 20 ms read chunk (frames) so the blocking read loop re-checks `stop` promptly.
const CHUNK_FRAMES: usize = CAPTURE_RATE as usize / 50;

/// Source names to try, in order: explicit env override, our shipped config drop-in's
/// name, then the common PipeWire/Pulse default. First that connects wins.
fn candidate_sources() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(s) = std::env::var("DONTSPEAK_AEC_SOURCE") {
        if !s.is_empty() {
            v.push(s);
        }
    }
    v.push("ds_ec_source".to_string());
    v.push("echo-cancel-source".to_string());
    v
}

/// Live echo-cancelled capture from the server's cancelled source. The `Simple` stream
/// lives entirely on the capture thread; this struct holds only cross-thread handles.
pub struct DuplexAudio {
    capture_rate: u32,
    /// Echo-cancelled mono f32, pushed by the capture thread, drained by the helper's
    /// concurrent-listen thread (via a [`CaptureHandle`]). Bounded to `CAPTURE_SECS`.
    cap: Arc<Mutex<VecDeque<f32>>>,
    /// Set on `Drop` to stop the capture thread.
    stop: Arc<AtomicBool>,
    /// Explicit stop/cancel signal (parity with macOS). Render is on rodio here, so this
    /// is informational only — the helper drains rodio directly.
    barge: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DuplexAudio {
    /// Open the cancelled source. Fail-quiet (`Err`) on any connect/format error; the caller
    /// then falls back to the half-duplex cpal + rodio path.
    pub fn open() -> Result<Self, String> {
        let spec = Spec {
            format: Format::F32le,
            channels: 1,
            rate: CAPTURE_RATE,
        };
        if !spec.is_valid() {
            return Err("invalid pulse sample spec".into());
        }

        // Connect on THIS thread so a failure (no cancelled source) returns synchronously and
        // the caller degrades immediately — no thread left spinning. Try each candidate name.
        let mut last_err = String::from("no echo-cancel source name to try");
        let mut connected: Option<(String, Simple)> = None;
        for name in candidate_sources() {
            match Simple::new(
                None,        // default server
                "DontSpeak", // app name
                Direction::Record,
                Some(&name),   // the cancelled source
                "aec-capture", // stream description
                &spec,
                None, // default channel map
                None, // default buffering
            ) {
                Ok(s) => {
                    connected = Some((name, s));
                    break;
                }
                Err(e) => last_err = format!("connect '{name}': {e}"),
            }
        }
        let (_name, simple) = connected.ok_or_else(|| {
            format!(
                "no PulseAudio/PipeWire echo-cancel source reachable ({last_err}) — load \
                 module-echo-cancel (see apps/linux/aec/) for full-duplex; using half-duplex"
            )
        })?;

        let cap: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let barge = Arc::new(AtomicBool::new(false));

        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let cap_t = cap.clone();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ds-pulse-aec".into())
            .spawn(move || capture_thread(simple, cap_t, stop_t, ready_tx))
            .map_err(|e| format!("spawn capture thread: {e}"))?;
        // The thread signals once it's entered the read loop (the stream is already open).
        let _ = ready_rx.recv();

        Ok(Self {
            capture_rate: CAPTURE_RATE,
            cap,
            stop,
            barge,
            thread: Some(thread),
        })
    }

    /// A `Send`+`Sync` handle to the echo-cancelled capture buffer, so the helper's
    /// concurrent listen thread can drain the mic while rodio renders TTS.
    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle::new(self.cap.clone(), self.capture_rate)
    }

    /// The negotiated capture sample rate. Drain a `capture_rate()`→16 kHz resampler
    /// before Parakeet.
    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// Capture-side AEC: the server's canceller references the render endpoint itself, so
    /// rodio keeps rendering TTS. We do NOT own render.
    pub fn owns_render(&self) -> bool {
        false
    }

    /// No-op: render stays on rodio (the server taps the render endpoint as the AEC
    /// reference, so we never feed PCM here).
    pub fn render_push(&self, _pcm_24k: &[f32]) {}

    /// Always empty (rodio renders, drained directly by the helper).
    pub fn render_pending(&self) -> bool {
        false
    }

    /// No-op (no render ring to flush; the helper stops the rodio player on barge).
    pub fn render_clear(&self) {}

    /// Drain the echo-cancelled mono f32 captured since the last call. Empty when no
    /// new audio has arrived.
    pub fn capture_drain(&self) -> Vec<f32> {
        let mut q = self.cap.lock().unwrap();
        q.drain(..).collect()
    }

    /// A `Send` barge handle for the explicit stop/cancel path (parity with macOS).
    pub fn barge_flag(&self) -> Arc<AtomicBool> {
        self.barge.clone()
    }
}

impl Drop for DuplexAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// `CaptureHandle` is shared with the Windows backend — see `crate::shared`.

/// The capture thread: blocking-read 20 ms chunks of echo-cancelled mono f32 and push them
/// into the shared buffer until `stop`. A server-side read error ends the loop quietly (the
/// next listen re-opens or degrades to half-duplex).
fn capture_thread(
    simple: Simple,
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<()>,
) {
    let _ = ready.send(()); // stream already open; unblock open()
    let cap_limit = CAPTURE_RATE as usize * CAPTURE_SECS;
    let mut bytes = vec![0u8; CHUNK_FRAMES * std::mem::size_of::<f32>()];
    let mut samples: Vec<f32> = Vec::with_capacity(CHUNK_FRAMES);
    while !stop.load(Ordering::Acquire) {
        if simple.read(&mut bytes).is_err() {
            break; // server gone / stream error — end capture
        }
        samples.clear();
        for f in bytes.chunks_exact(4) {
            samples.push(f32::from_le_bytes([f[0], f[1], f[2], f[3]]));
        }
        enqueue_bounded(&cap, &samples, cap_limit);
    }
}
