//! Windows WASAPI "Communications" duplex audio (capture-side AEC).
//!
//! Unlike macOS VPIO (which owns BOTH render and capture on one clock), Windows
//! native AEC is **capture-side**: the OS audio engine's Communications APO (+ Win11
//! Voice Clarity) taps the system render endpoint as the echo reference *itself*. So
//! we do NOT route TTS through this unit — rodio keeps rendering normally
//! ([`owns_render`](DuplexAudio::owns_render) is `false`) and this backend only opens
//! an echo-cancelled microphone stream.
//!
//! The trick is opening the capture client in the Communications category
//! (`IAudioClient2::SetClientProperties` with `AudioCategory_Communications` BEFORE
//! `Initialize`), which engages the capture-side AEC APO. We must NOT set
//! `AUDCLNT_STREAMOPTIONS_RAW` — RAW opts *out* of all processing. `cpal` cannot set
//! `SetClientProperties`, which is why this is a direct WASAPI capture rather than a
//! cpal stream.
//!
//! Threading: a dedicated thread does ALL the COM work (apartment-local) — it
//! negotiates the format, runs the event-driven capture loop, and pushes
//! echo-cancelled mono f32 into a shared buffer. `open()` blocks until that thread
//! reports the negotiated rate (or an error, → half-duplex). Any COM/format failure
//! is fail-quiet: `open()` returns `Err` and the caller degrades to half-duplex.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::shared::{CaptureHandle, enqueue_bounded};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMOPTIONS_NONE, AudioCategory_Communications, AudioClientProperties,
    IAudioCaptureClient, IAudioClient2, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVEFORMATEXTENSIBLE, eCapture, eCommunications,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// WAVEFORMATEX::wFormatTag values we care about.
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Cap the capture buffer at ~2 s of audio. The helper drains it every poll tick;
/// if a listen stalls we drop the OLDEST samples rather than grow unbounded.
const CAPTURE_SECS: usize = 2;

/// Live echo-cancelled WASAPI capture. The COM objects live entirely on the capture
/// thread; this struct only holds the cross-thread handles (the drained buffer, the
/// stop/barge flags, the join handle).
pub struct DuplexAudio {
    capture_rate: u32,
    /// Echo-cancelled mono f32, pushed by the capture thread, drained by the helper's
    /// concurrent-listen thread (via a [`CaptureHandle`]). Bounded to `CAPTURE_SECS`.
    cap: Arc<Mutex<VecDeque<f32>>>,
    /// Set on `Drop` to stop the capture thread.
    stop: Arc<AtomicBool>,
    /// Explicit stop/cancel signal (parity with the macOS barge flag). Render is on
    /// rodio here, so this is informational only — the helper drains rodio directly.
    barge: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DuplexAudio {
    /// Open an echo-cancelled WASAPI capture stream in the Communications category.
    /// Returns an error string (fail-quiet logging) on any COM/format error; the
    /// caller then falls back to the half-duplex cpal + rodio path.
    pub fn open() -> Result<Self, String> {
        let cap: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let barge = Arc::new(AtomicBool::new(false));

        // The thread reports the negotiated rate (or an open error) back here so
        // `open()` can return synchronously, matching the macOS contract.
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let cap_t = cap.clone();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ds-wasapi-aec".into())
            .spawn(move || capture_thread(cap_t, stop_t, tx))
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match rx.recv() {
            Ok(Ok(rate)) => Ok(Self {
                capture_rate: rate,
                cap,
                stop,
                barge,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // Thread died before reporting — treat as open failure.
                let _ = thread.join();
                Err("WASAPI capture thread exited before init".into())
            }
        }
    }

    /// A `Send`+`Sync` handle to the echo-cancelled capture buffer, so the helper's
    /// concurrent listen thread can drain the mic while rodio renders TTS.
    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle::new(self.cap.clone(), self.capture_rate)
    }

    /// The negotiated capture sample rate (the WASAPI mix-format rate). Drain a
    /// `capture_rate()`→16 kHz resampler before Parakeet.
    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// Capture-side AEC: the OS Communications APO references the system render
    /// endpoint itself, so rodio keeps rendering TTS. We do NOT own render.
    pub fn owns_render(&self) -> bool {
        false
    }

    /// No-op: render stays on rodio (the OS taps the render endpoint as the AEC
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

// `CaptureHandle` is shared with the Linux backend — see `crate::shared`.

/// The capture thread: init COM, open the Communications-category capture client,
/// negotiate the format, report the rate back, then run the event-driven loop until
/// `stop`. All COM objects stay local to this thread (the apartment is thread-bound).
fn capture_thread(
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<u32, String>>,
) {
    unsafe {
        // MTA on this thread. S_OK/S_FALSE ⇒ we balance with CoUninitialize;
        // RPC_E_CHANGED_MODE (err) ⇒ COM already up elsewhere — proceed, don't uninit.
        let did_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let run = capture_run(&cap, &stop, &tx);
        if let Err(e) = run {
            // If we never reported success, the error reaches `open()`; if we already
            // reported a rate, the channel is closed and this send is a harmless no-op.
            let _ = tx.send(Err(e));
        }
        if did_init {
            CoUninitialize();
        }
    }
}

/// Inner capture body (COM error → `windows::core::Error` mapped to a string). On
/// success it sends the rate, runs the loop, and returns `Ok(())` at `stop`.
unsafe fn capture_run(
    cap: &Arc<Mutex<VecDeque<f32>>>,
    stop: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Result<u32, String>>,
) -> Result<(), String> {
    // `&'static str` context so the returned closure owns no borrow (call sites pass
    // string literals).
    let map = |ctx: &'static str| move |e: windows::core::Error| format!("{ctx}: {e}");
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(map("enumerator"))?;
        // The Communications capture endpoint (the comms-tuned default mic).
        let device = enumerator
            .GetDefaultAudioEndpoint(eCapture, eCommunications)
            .map_err(map("default capture endpoint"))?;
        let client: IAudioClient2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(map("activate IAudioClient2"))?;

        // Communications category BEFORE Initialize → engages the capture-side AEC APO
        // (+ Win11 Voice Clarity). `Options: NONE` (NOT RAW — RAW opts out of processing).
        let props = AudioClientProperties {
            cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
            bIsOffload: false.into(),
            eCategory: AudioCategory_Communications,
            Options: AUDCLNT_STREAMOPTIONS_NONE,
        };
        client
            .SetClientProperties(&props)
            .map_err(map("set communications category"))?;

        // Negotiated shared-mode mix format (usually 48 kHz float, 1–2 ch).
        let pwfx = client.GetMixFormat().map_err(map("get mix format"))?;
        if pwfx.is_null() {
            return Err("mix format is null".into());
        }
        let wfx = &*pwfx;
        let rate = wfx.nSamplesPerSec;
        let channels = wfx.nChannels as usize;
        let bits = wfx.wBitsPerSample;
        // Float vs PCM: tag 3 = IEEE float; for EXTENSIBLE inspect the SubFormat GUID
        // (Data1 == 3 ⇒ IEEE float, == 1 ⇒ PCM). Anything else we treat by bit depth.
        let is_float = match wfx.wFormatTag {
            WAVE_FORMAT_IEEE_FLOAT => true,
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_EXTENSIBLE => {
                let ext = &*(pwfx as *const WAVEFORMATEXTENSIBLE);
                ext.SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT as u32
            }
            _ => bits == 32, // best guess
        };
        if channels == 0 || (bits != 16 && bits != 32) {
            CoTaskMemFree(Some(pwfx as *const _));
            return Err(format!(
                "unsupported mix format ({bits} bit, {channels} ch)"
            ));
        }

        // Event-driven shared-mode capture. Buffer duration 0 ⇒ the engine uses its
        // default device period (the event fires once per period).
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            0,
            0,
            pwfx,
            None,
        );
        CoTaskMemFree(Some(pwfx as *const _)); // engine copied the format
        init.map_err(map("initialize capture client"))?;

        let event: HANDLE = CreateEventW(None, false, false, None).map_err(map("create event"))?;
        client
            .SetEventHandle(event)
            .map_err(map("set event handle"))?;

        let capture_client: IAudioCaptureClient =
            client.GetService().map_err(map("get capture service"))?;
        client.Start().map_err(map("start capture"))?;

        // Report success — `open()` returns now.
        let _ = tx.send(Ok(rate));

        let cap_limit = rate as usize * CAPTURE_SECS;
        let mut acc: Vec<f32> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            // Wake on the period event (200 ms guard so we re-check `stop`).
            if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                continue;
            }
            // Drain every queued packet.
            loop {
                let packet = capture_client
                    .GetNextPacketSize()
                    .map_err(map("next packet size"))?;
                if packet == 0 {
                    break;
                }
                let mut pdata: *mut u8 = std::ptr::null_mut();
                let mut nframes: u32 = 0;
                let mut flags: u32 = 0;
                capture_client
                    .GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None)
                    .map_err(map("get buffer"))?;
                let frames = nframes as usize;
                acc.clear();
                acc.reserve(frames);
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || pdata.is_null() {
                    acc.resize(frames, 0.0);
                } else {
                    downmix(pdata, frames, channels, is_float, &mut acc);
                }
                capture_client
                    .ReleaseBuffer(nframes)
                    .map_err(map("release buffer"))?;

                // Push to the shared buffer, dropping oldest if a listen stalls.
                enqueue_bounded(cap, &acc, cap_limit);
            }
        }

        let _ = client.Stop();
        let _ = CloseHandle(event);
        Ok(())
    }
}

/// Downmix an interleaved WASAPI packet (`frames` × `channels`, float or i16) to
/// mono f32, appending to `out`.
unsafe fn downmix(
    pdata: *mut u8,
    frames: usize,
    channels: usize,
    is_float: bool,
    out: &mut Vec<f32>,
) {
    let total = frames * channels;
    let inv = 1.0 / channels as f32;
    if is_float {
        let s = unsafe { std::slice::from_raw_parts(pdata as *const f32, total) };
        for f in 0..frames {
            let base = f * channels;
            let mut sum = 0.0f32;
            for c in 0..channels {
                sum += s[base + c];
            }
            out.push(sum * inv);
        }
    } else {
        let s = unsafe { std::slice::from_raw_parts(pdata as *const i16, total) };
        for f in 0..frames {
            let base = f * channels;
            let mut sum = 0.0f32;
            for c in 0..channels {
                sum += s[base + c] as f32 / 32768.0;
            }
            out.push(sum * inv);
        }
    }
}
