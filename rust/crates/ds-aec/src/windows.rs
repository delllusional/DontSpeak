//! Windows WASAPI Communications capture-side AEC.
//!
//! Unlike macOS VPIO, AEC is **capture-side**: Communications APO (+ Win11 Voice Clarity)
//! taps the render endpoint as echo reference. TTS stays on rodio (`owns_render() == false`).
//!
//! Open capture with `AudioCategory_Communications` BEFORE `Initialize` (not RAW —
//! RAW opts out of processing). cpal can't SetClientProperties → direct WASAPI.
//!
//! Dedicated COM thread negotiates format, runs event-driven capture, pushes mono f32.
//! `open()` waits for rate or Err → half-duplex.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::shared::{CaptureHandle, RenderHandle, enqueue_bounded};

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

/// ~2 s capture cap; drop oldest if listen stalls.
const CAPTURE_SECS: usize = 2;

/// Live echo-cancelled WASAPI capture. COM stays on the capture thread; this holds
/// cross-thread handles only.
pub struct DuplexAudio {
    capture_rate: u32,
    /// Mono f32 from capture thread; drained via [`CaptureHandle`]. Bounded to `CAPTURE_SECS`.
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    /// Stop/cancel (macOS barge parity); render is rodio — informational only.
    barge: Arc<AtomicBool>,
    /// Mid-stream failure while reconnecting (not open() failures). See [`Self::last_error`].
    last_error: Arc<Mutex<Option<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DuplexAudio {
    /// Open Communications-category capture. COM/format error → half-duplex.
    pub fn open() -> Result<Self, String> {
        let cap: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let barge = Arc::new(AtomicBool::new(false));
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // Sync open() with macOS contract: wait for rate or error.
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
                let _ = thread.join();
                Err("WASAPI capture thread exited before init".into())
            }
        }
    }

    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle::new(self.cap.clone(), self.capture_rate)
    }

    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// Capture-side: OS references render endpoint; rodio keeps TTS.
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

    pub fn set_muted(&self, _on: bool) {}

    /// No-op (rodio owns output); keeps helper feeder path cfg-free.
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

    /// Mid-stream capture failure while degraded/reconnecting (`None` after success).
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

/// COM-init + Communications capture + event loop until `stop`. COM stays on this thread.
///
/// Mid-stream errors (e.g. device unplug) reconnect with backoff instead of killing the
/// thread forever. First-open failure is reported via `tx` (no retry) so open() → half-duplex.
fn capture_thread(
    cap: Arc<Mutex<VecDeque<f32>>>,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<u32, String>>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    // SAFETY: COM FFI on this capture thread only; CoUninitialize only if did_init.
    unsafe {
        // MTA: S_OK/S_FALSE → we uninit; RPC_E_CHANGED_MODE → COM already up, don't uninit.
        let did_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();

        let mut opened = match open_capture() {
            Ok(o) => {
                let _ = tx.send(Ok(o.rate));
                o
            }
            Err(e) => {
                // First open only — report to open(), no retry.
                let _ = tx.send(Err(e));
                if did_init {
                    CoUninitialize();
                }
                return;
            }
        };
        let published_rate = opened.rate;
        let mut rate_converter = None;
        // Last sample into published-rate stream — seeds first rate-changing reconnect.
        let mut stream_tail = None;

        while !stop.load(Ordering::Acquire) {
            match run_capture_loop(
                &opened,
                &cap,
                &stop,
                published_rate,
                rate_converter.as_mut(),
                &mut stream_tail,
            ) {
                Ok(()) => break, // `stop` was set — clean shutdown
                Err(e) => {
                    log::warn!(
                        target: "aec",
                        "WASAPI echo-cancelled capture lost ({e}) — \
                         reconnecting to the default communications endpoint"
                    );
                    *last_error.lock().unwrap() = Some(e);
                    drop(opened);

                    let reopened = loop {
                        if stop.load(Ordering::Acquire) {
                            break None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        match open_capture() {
                            Ok(o) => break Some(o),
                            Err(e2) => {
                                log::warn!(
                                    target: "aec",
                                    "WASAPI echo-cancel reconnect attempt failed \
                                     ({e2}); retrying"
                                );
                                *last_error.lock().unwrap() = Some(e2);
                            }
                        }
                    };
                    match reopened {
                        Some(o) => {
                            log::info!(
                                target: "aec",
                                "WASAPI echo-cancelled capture reconnected ({} Hz)",
                                o.rate
                            );
                            if o.rate != published_rate {
                                log::info!(
                                    target: "aec",
                                    "WASAPI echo-cancel reconnect renegotiated \
                                     {published_rate} Hz -> {} Hz; converting continuously back \
                                     to {published_rate} Hz",
                                    o.rate
                                );
                            }
                            rate_converter = (o.rate != published_rate).then(|| {
                                let mut rs =
                                    crate::resample::LinearResampler::new(o.rate, published_rate);
                                // Seed to avoid click at resume.
                                if let Some(prev) = stream_tail {
                                    rs.seed_prev(prev);
                                }
                                rs
                            });
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

/// One live capture session; Drop stops client + closes event (reconnect-safe).
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
        // SAFETY: client owned here; event from CreateEventW — closed once here.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Default Communications capture endpoint, mix format, start client.
/// Fresh enum/device/client on every call (initial open + reconnect).
unsafe fn open_capture() -> Result<OpenedCapture, String> {
    let map = |ctx: &'static str| move |e: windows::core::Error| format!("{ctx}: {e}");
    // SAFETY: COM-init capture thread. pwfx null-checked; EXTENSIBLE only when tag says so;
    // CoTaskMemFree after Initialize copies format; event owned by OpenedCapture::Drop.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(map("enumerator"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eCapture, eCommunications)
            .map_err(map("default capture endpoint"))?;
        let client: IAudioClient2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(map("activate IAudioClient2"))?;

        // Communications BEFORE Initialize → AEC APO. Options NONE (not RAW).
        let props = AudioClientProperties {
            cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
            bIsOffload: false.into(),
            eCategory: AudioCategory_Communications,
            Options: AUDCLNT_STREAMOPTIONS_NONE,
        };
        client
            .SetClientProperties(&props)
            .map_err(map("set communications category"))?;

        let pwfx = client.GetMixFormat().map_err(map("get mix format"))?;
        if pwfx.is_null() {
            return Err("mix format is null".into());
        }
        let wfx = &*pwfx;
        let rate = wfx.nSamplesPerSec;
        let channels = wfx.nChannels as usize;
        let bits = wfx.wBitsPerSample;
        // Float vs PCM: tag / EXTENSIBLE SubFormat; else bit depth.
        let is_float = match wfx.wFormatTag {
            WAVE_FORMAT_IEEE_FLOAT => true,
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_EXTENSIBLE => {
                let ext = &*(pwfx as *const WAVEFORMATEXTENSIBLE);
                ext.SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT as u32
            }
            _ => bits == 32, // best guess
        };
        if channels == 0 || (is_float && bits != 32) || (!is_float && bits != 16) {
            CoTaskMemFree(Some(pwfx as *const _));
            return Err(format!(
                "unsupported mix format ({bits} bit, {channels} ch)"
            ));
        }

        // Event-driven shared mode; buffer 0 → default device period.
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

/// Event-driven read until `stop` (`Ok`) or mid-stream COM error (`Err` → reconnect).
unsafe fn run_capture_loop(
    opened: &OpenedCapture,
    cap: &Arc<Mutex<VecDeque<f32>>>,
    stop: &Arc<AtomicBool>,
    published_rate: u32,
    mut rate_converter: Option<&mut crate::resample::LinearResampler>,
    stream_tail: &mut Option<f32>,
) -> Result<(), String> {
    let map = |ctx: &'static str| move |e: windows::core::Error| format!("{ctx}: {e}");
    // SAFETY: live session COM on this thread; GetBuffer/ReleaseBuffer paired;
    // pdata only when non-null for nframes.
    unsafe {
        let cap_limit = published_rate as usize * CAPTURE_SECS;
        let mut acc: Vec<f32> = Vec::new();
        let mut converted: Vec<f32> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            // Period event; 200 ms so we re-check stop.
            if WaitForSingleObject(opened.event, 200) != WAIT_OBJECT_0 {
                continue;
            }
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

                if let Some(converter) = rate_converter.as_deref_mut() {
                    converted.clear();
                    converter.process(&acc, &mut converted);
                    *stream_tail = converter.last_sample();
                    enqueue_bounded(cap, &converted, cap_limit);
                } else {
                    if let Some(&last) = acc.last() {
                        *stream_tail = Some(last);
                    }
                    enqueue_bounded(cap, &acc, cap_limit);
                }
            }
        }
        Ok(())
    }
}

/// Interleaved WASAPI packet → mono f32 appended to `out`.
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
        // SAFETY: GetBuffer packet of frames*channels f32 until ReleaseBuffer.
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
        // SAFETY: same packet lifetime; interleaved i16 PCM.
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
