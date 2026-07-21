//! Linux PipeWire/PulseAudio `module-echo-cancel` (capture-side AEC).
//!
//! Like Windows WASAPI (unlike macOS VPIO), AEC is **capture-side**: the sound server's
//! WebRTC canceller in `module-echo-cancel` exposes a cancelled virtual source that
//! references the render endpoint. TTS stays on rodio (`owns_render() == false`); this
//! backend only opens that source.
//!
//! Opened via the Pulse simple API (works with Pulse and PipeWire/`pipewire-pulse`).
//! Source name: `$DONTSPEAK_AEC_SOURCE`, else `ds_ec_source`, else `echo-cancel-source`.
//! Connect/format failure → `open()` Err → half-duplex. (In-process WebRTC APM is a
//! future option.)
//!
//! Dedicated thread owns the blocking `Simple` stream, reads ~20 ms chunks, pushes mono
//! f32 into a bounded buffer a [`CaptureHandle`] drains.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::shared::{CaptureHandle, RenderHandle, enqueue_bounded};

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

/// Ask for 48 kHz mono f32 (server resamples cancelled source); helper does 48k→16k.
const CAPTURE_RATE: u32 = 48_000;
/// ~2 s cap; drop oldest if listen stalls.
const CAPTURE_SECS: usize = 2;
/// 20 ms read chunk so the blocking loop re-checks `stop` promptly.
const CHUNK_FRAMES: usize = CAPTURE_RATE as usize / 50;

/// Try order: env override, shipped drop-in name, common default. First connect wins.
fn candidate_sources() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(s) = std::env::var("DONTSPEAK_AEC_SOURCE")
        && !s.is_empty()
    {
        v.push(s);
    }
    v.push("ds_ec_source".to_string());
    v.push("echo-cancel-source".to_string());
    v
}

fn connect_capture(spec: &Spec) -> Result<(String, Simple), String> {
    let mut last_err = String::from("no echo-cancel source name to try");
    for name in candidate_sources() {
        match Simple::new(
            None,
            "DontSpeak",
            Direction::Record,
            Some(&name),
            "aec-capture",
            spec,
            None,
            None,
        ) {
            Ok(simple) => return Ok((name, simple)),
            Err(e) => last_err = format!("connect '{name}': {e}"),
        }
    }
    Err(last_err)
}

/// Live echo-cancelled capture. `Simple` lives on the capture thread; this holds
/// cross-thread handles only.
pub struct DuplexAudio {
    capture_rate: u32,
    /// Mono f32 from capture thread; drained via [`CaptureHandle`]. Bounded to `CAPTURE_SECS`.
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    /// Stop/cancel signal (parity with macOS); render is rodio — informational only.
    barge: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DuplexAudio {
    /// Open cancelled source. Fail-quiet `Err` → half-duplex cpal + rodio.
    pub fn open() -> Result<Self, String> {
        let spec = Spec {
            format: Format::F32le,
            channels: 1,
            rate: CAPTURE_RATE,
        };
        if !spec.is_valid() {
            return Err("invalid pulse sample spec".into());
        }

        // Connect on this thread so failure returns synchronously (no orphan thread).
        let (_name, simple) = connect_capture(&spec).map_err(|last_err| {
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
        let _ = ready_rx.recv();

        Ok(Self {
            capture_rate: CAPTURE_RATE,
            cap,
            stop,
            barge,
            thread: Some(thread),
        })
    }

    /// Send+Sync handle so concurrent listen can drain while rodio renders.
    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle::new(self.cap.clone(), self.capture_rate)
    }

    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// Capture-side: server references render endpoint; rodio keeps TTS. No render ownership.
    pub fn owns_render(&self) -> bool {
        false
    }

    pub fn render_push(&self, _pcm_24k: &[f32]) {}

    pub fn render_pending(&self) -> bool {
        false
    }

    pub fn render_buffered(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    pub fn render_clear(&self) {}

    /// Mute is macOS-only; rodio player volume mutes here.
    pub fn set_muted(&self, _on: bool) {}

    /// No-op render handle — keeps helper feeder path cfg-free.
    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle::new()
    }

    pub fn capture_drain(&self) -> Vec<f32> {
        let mut q = self.cap.lock().unwrap();
        q.drain(..).collect()
    }

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

/// Blocking-read 20 ms mono f32 chunks into the shared buffer until `stop`.
/// Read error → reconnect with backoff (or stop).
fn capture_thread(
    mut simple: Simple,
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<()>,
) {
    let _ = ready.send(()); // stream already open; unblock open()
    let cap_limit = CAPTURE_RATE as usize * CAPTURE_SECS;
    let spec = Spec {
        format: Format::F32le,
        channels: 1,
        rate: CAPTURE_RATE,
    };
    let mut bytes = vec![0u8; CHUNK_FRAMES * std::mem::size_of::<f32>()];
    let mut samples: Vec<f32> = Vec::with_capacity(CHUNK_FRAMES);
    while !stop.load(Ordering::Acquire) {
        if let Err(e) = simple.read(&mut bytes) {
            log::warn!(target: "aec", "PulseAudio/PipeWire AEC read failed ({e}); reconnecting");
            loop {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                match connect_capture(&spec) {
                    Ok((name, reopened)) => {
                        log::info!(target: "aec", "PulseAudio/PipeWire AEC reconnected to {name}");
                        simple = reopened;
                        break;
                    }
                    Err(retry) => {
                        log::warn!(target: "aec", "PulseAudio/PipeWire AEC reconnect failed ({retry})");
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                }
            }
            continue;
        }
        samples.clear();
        for f in bytes.chunks_exact(4) {
            samples.push(f32::from_le_bytes([f[0], f[1], f[2], f[3]]));
        }
        enqueue_bounded(&cap, &samples, cap_limit);
    }
}

#[cfg(test)]
mod candidate_sources_tests {
    use super::*;

    /// `candidate_sources` reads process env — serialize mutations across concurrent tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const VAR: &str = "DONTSPEAK_AEC_SOURCE";

    #[test]
    fn no_override_falls_back_to_both_defaults_in_order() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env mutation under ENV_LOCK.
        unsafe { std::env::remove_var(VAR) };
        assert_eq!(
            candidate_sources(),
            vec!["ds_ec_source".to_string(), "echo-cancel-source".to_string()]
        );
    }

    #[test]
    fn explicit_override_is_tried_first_then_both_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env mutation under ENV_LOCK; cleared below.
        unsafe { std::env::set_var(VAR, "my-custom-source") };
        let got = candidate_sources();
        // SAFETY: still under ENV_LOCK.
        unsafe { std::env::remove_var(VAR) };
        assert_eq!(
            got,
            vec![
                "my-custom-source".to_string(),
                "ds_ec_source".to_string(),
                "echo-cancel-source".to_string(),
            ]
        );
    }

    #[test]
    fn empty_override_is_guarded_out_not_tried_as_a_source_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env mutation under ENV_LOCK; cleared below.
        unsafe { std::env::set_var(VAR, "") };
        let got = candidate_sources();
        // SAFETY: still under ENV_LOCK.
        unsafe { std::env::remove_var(VAR) };
        assert_eq!(
            got,
            vec!["ds_ec_source".to_string(), "echo-cancel-source".to_string()]
        );
    }
}
