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
    /// The most recent mid-stream capture failure (e.g. a device unplug), if any,
    /// set by the capture thread and cleared on a successful reconnect. `open()`
    /// failures are NOT recorded here — those already return synchronously via
    /// `open()`'s `Result`. Lets a caller poll for "capture is degraded/reconnecting"
    /// instead of only finding out by silence (see [`Self::last_error`]).
    last_error: Arc<Mutex<Option<String>>>,
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
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // The thread reports the negotiated rate (or an open error) back here so
        // `open()` can return synchronously, matching the macOS contract.
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let cap_t = cap.clone();
        let stop_t = stop.clone();
        let err_t = last_error.clone();
        let thread = std::thread::Builder::new()
            .name("ds-wasapi-aec".into())
            .spawn(move || capture_thread(cap_t, stop_t, tx, err_t))
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match rx.recv() {
            Ok(Ok(rate)) => Ok(Self {
                capture_rate: rate,
                cap,
                stop,
                barge,
                last_error,
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

    /// The most recent mid-stream capture failure (e.g. the capture device was
    /// unplugged and `AUDCLNT_E_DEVICE_INVALIDATED` came back), if the capture
    /// thread is currently degraded/reconnecting. `None` once a reconnect
    /// succeeds. Lets a caller surface "echo-cancelled capture dropped out" in a
    /// status/health display instead of only noticing dictation went silent.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
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
///
/// A mid-stream COM error (e.g. `AUDCLNT_E_DEVICE_INVALIDATED` on a capture device
/// unplug) used to propagate straight out of the loop via `?` and end the thread —
/// silently and permanently killing echo-cancelled capture with zero logging and no
/// recovery (full-duplex dictation went silently deaf until the whole helper
/// process restarted). Now such an error is logged and the thread re-opens the
/// default communications endpoint and resumes, retrying with a short backoff
/// until `stop` is set. An error while establishing the VERY FIRST connection is
/// NOT retried here — it is reported back through `tx` so `open()` returns
/// synchronously and the caller falls back to half-duplex immediately, unchanged.
fn capture_thread(
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<u32, String>>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    // SAFETY: COM FFI confined to this dedicated capture thread — CoInitializeEx runs
    // first, every COM object created below (via open_capture) lives and dies on this
    // thread, and CoUninitialize only balances an init that returned S_OK/S_FALSE
    // (`did_init`).
    unsafe {
        // MTA on this thread. S_OK/S_FALSE ⇒ we balance with CoUninitialize;
        // RPC_E_CHANGED_MODE (err) ⇒ COM already up elsewhere — proceed, don't uninit.
        let did_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();

        let mut opened = match open_capture() {
            Ok(o) => {
                let _ = tx.send(Ok(o.rate));
                o
            }
            Err(e) => {
                // Initial open failure: report it out (this reaches `open()`; the
                // caller degrades to half-duplex) and do not retry.
                let _ = tx.send(Err(e));
                if did_init {
                    CoUninitialize();
                }
                return;
            }
        };

        while !stop.load(Ordering::Acquire) {
            match run_capture_loop(&opened, &cap, &stop) {
                Ok(()) => break, // `stop` was set — clean shutdown
                Err(e) => {
                    eprintln!(
                        "dontspeak: WASAPI echo-cancelled capture lost ({e}) — \
                         reconnecting to the default communications endpoint"
                    );
                    *last_error.lock().unwrap() = Some(e);
                    let original_rate = opened.rate;
                    // Drop the failed COM objects (Stop + CloseHandle) before
                    // reopening.
                    drop(opened);

                    let reopened = loop {
                        if stop.load(Ordering::Acquire) {
                            break None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        match open_capture() {
                            Ok(o) => break Some(o),
                            Err(e2) => {
                                eprintln!(
                                    "dontspeak: WASAPI echo-cancel reconnect attempt failed \
                                     ({e2}); retrying"
                                );
                                *last_error.lock().unwrap() = Some(e2);
                            }
                        }
                    };
                    match reopened {
                        Some(o) => {
                            eprintln!(
                                "dontspeak: WASAPI echo-cancelled capture reconnected \
                                 ({} Hz)",
                                o.rate
                            );
                            if o.rate != original_rate {
                                // `capture_rate()` on the `DuplexAudio` handle is fixed at
                                // the rate negotiated on the FIRST open; a reconnect that
                                // renegotiates a different rate would silently mis-resample
                                // downstream. Surface it loudly rather than let audio quietly
                                // degrade — this is a known limitation of the reconnect path.
                                eprintln!(
                                    "dontspeak: WASAPI echo-cancel reconnect renegotiated \
                                     {original_rate} Hz -> {} Hz; downstream resampling still \
                                     assumes {original_rate} Hz for this session (restart the \
                                     helper to pick up the new rate cleanly)",
                                    o.rate
                                );
                            }
                            *last_error.lock().unwrap() = None;
                            opened = o;
                        }
                        None => break, // `stop` was set while reconnecting
                    }
                }
            }
        }

        if did_init {
            CoUninitialize();
        }
    }
}

/// The COM objects for one live capture session. `Drop` stops the client and
/// closes the event handle, so replacing `opened` on a reconnect (or falling off
/// the end of `capture_thread`) always tears the old session down cleanly.
struct OpenedCapture {
    client: IAudioClient2,
    capture_client: IAudioCaptureClient,
    event: HANDLE,
    rate: u32,
    channels: usize,
    is_float: bool,
}

impl Drop for OpenedCapture {
    fn drop(&mut self) {
        // SAFETY: `client` is a live COM interface owned by this struct, and `event` is
        // the handle CreateEventW returned in open_capture — closed exactly once, here.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Enumerate the default Communications-category capture endpoint, negotiate its
/// mix format, and start the capture client. Called once for the initial open and
/// again (from scratch — a fresh enumerator/device/client) for every reconnect
/// attempt after a mid-stream failure, so a replaced/re-plugged device is picked
/// up the same way a first launch would.
unsafe fn open_capture() -> Result<OpenedCapture, String> {
    // `&'static str` context so the returned closure owns no borrow (call sites pass
    // string literals).
    let map = |ctx: &'static str| move |e: windows::core::Error| format!("{ctx}: {e}");
    // SAFETY: COM/WASAPI FFI on the caller's (COM-initialized) capture thread. `pwfx`
    // is null-checked before the `&*pwfx` deref, reinterpreted as WAVEFORMATEXTENSIBLE
    // only when wFormatTag says the buffer has that layout, and freed via CoTaskMemFree
    // on every path once Initialize has copied it; `event` ownership moves into
    // OpenedCapture, whose Drop closes it.
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

        Ok(OpenedCapture {
            client,
            capture_client,
            event,
            rate,
            channels,
            is_float,
        })
    }
}

/// Run the event-driven read loop against an already-open capture session until
/// `stop` is set (returns `Ok(())`) or a COM call fails mid-stream (returns
/// `Err`, e.g. `AUDCLNT_E_DEVICE_INVALIDATED` on a device unplug) — the caller
/// then logs it and reconnects via [`open_capture`].
unsafe fn run_capture_loop(
    opened: &OpenedCapture,
    cap: &Arc<Mutex<VecDeque<f32>>>,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    let map = |ctx: &'static str| move |e: windows::core::Error| format!("{ctx}: {e}");
    // SAFETY: WASAPI FFI against the live session in `opened` (COM initialized on this
    // thread by capture_thread). GetBuffer's out-params are stack locals it fills
    // before we read them, every GetBuffer is paired with a ReleaseBuffer, and `pdata`
    // is dereferenced (in downmix) only when non-null, for exactly the `nframes` the
    // driver reported.
    unsafe {
        let cap_limit = opened.rate as usize * CAPTURE_SECS;
        let mut acc: Vec<f32> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            // Wake on the period event (200 ms guard so we re-check `stop`).
            if WaitForSingleObject(opened.event, 200) != WAIT_OBJECT_0 {
                continue;
            }
            // Drain every queued packet.
            loop {
                let packet = opened
                    .capture_client
                    .GetNextPacketSize()
                    .map_err(map("next packet size"))?;
                if packet == 0 {
                    break;
                }
                let mut pdata: *mut u8 = std::ptr::null_mut();
                let mut nframes: u32 = 0;
                let mut flags: u32 = 0;
                opened
                    .capture_client
                    .GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None)
                    .map_err(map("get buffer"))?;
                let frames = nframes as usize;
                acc.clear();
                acc.reserve(frames);
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || pdata.is_null() {
                    acc.resize(frames, 0.0);
                } else {
                    downmix(pdata, frames, opened.channels, opened.is_float, &mut acc);
                }
                opened
                    .capture_client
                    .ReleaseBuffer(nframes)
                    .map_err(map("release buffer"))?;

                // Push to the shared buffer, dropping oldest if a listen stalls.
                enqueue_bounded(cap, &acc, cap_limit);
            }
        }
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
        // SAFETY: the caller got `pdata` + `frames` from GetBuffer — a valid packet of
        // `frames * channels` interleaved f32 samples (the negotiated format when
        // `is_float`), alive until ReleaseBuffer runs after this returns.
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
        // SAFETY: as above, but the negotiated format is 16-bit PCM, so the packet
        // holds `frames * channels` interleaved i16 samples.
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
