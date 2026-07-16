//! macOS Voice-Processing I/O duplex audio.
//!
//! One `kAudioUnitSubType_VoiceProcessingIO` unit owns speaker + mic; Apple's voice
//! processing cancels render from capture on one clock (no delay/drift alignment).
//!
//! RT render/input callbacks must not block — lock-free SPSC rings (ringbuf):
//! `play` (helper pushes 24 kHz→unit-rate; render drains) and `cap` (input pushes
//! AEC-cleaned mic; helper drains). Helper ends behind Mutex (RT never locks).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{Args, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use objc2_audio_toolbox::{kAudioOutputUnitProperty_EnableIO, kAudioUnitProperty_StreamFormat};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::resample::LinearResampler;

/// Kokoro synth rate (`ds_tts::vocab::SAMPLE_RATE`).
const SYNTH_RATE: u32 = 24_000;

/// VPIO rate both directions (de-facto 48 kHz). Forced other rate → open fails → half-duplex.
const UNIT_RATE: u32 = 48_000;

/// Render ring. Feeder paces ≤100 ms against ~2 s lookahead; 90 s is tail-drop headroom.
const RENDER_CAP: usize = (UNIT_RATE * 90) as usize;

/// Capture ring ~2 s (helper drains every poll tick).
const CAPTURE_CAP: usize = (UNIT_RATE * 2) as usize;

/// Live echo-cancelled duplex unit. `!Send` (AudioUnit): open/drive on one thread.
pub struct DuplexAudio {
    /// Drop stops capture+render.
    _unit: AudioUnit,
    capture_rate: u32,
    /// Helper-side render producer + 24 kHz→unit-rate resampler; Arc for [`RenderHandle`].
    play: Arc<Mutex<(HeapProd<f32>, LinearResampler)>>,
    /// Helper-side capture consumer; Arc for concurrent listen via [`CaptureHandle`].
    cap: Arc<Mutex<HeapCons<f32>>>,
    /// `render_clear` sets this; RT callback drains next tick (atomic — no RT lock).
    flush: Arc<AtomicBool>,
    /// Even = idle; odd = callback draining. Prevents push mistaking "claimed" for "done".
    render_gen: Arc<AtomicU64>,
    /// Render mute: wall-rate consume + zero-fill. See [`set_muted`](Self::set_muted).
    mute: Arc<AtomicBool>,
}

impl DuplexAudio {
    /// Open VPIO (mic + speaker, AEC on). CoreAudio error → half-duplex.
    pub fn open() -> Result<Self, String> {
        // EnableIO + StreamFormat before Initialize.
        let mut unit = AudioUnit::new_uninitialized(IOType::VoiceProcessingIO)
            .map_err(|e| format!("VPIO new: {e}"))?;

        let enable: u32 = 1;
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Input,
            Element::Input,
            Some(&enable),
        )
        .map_err(|e| format!("enable mic (input bus 1): {e}"))?;
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Output,
            Element::Output,
            Some(&enable),
        )
        .map_err(|e| format!("enable speaker (output bus 0): {e}"))?;

        // Mono f32 non-interleaved packed — both directions.
        let fmt = StreamFormat {
            sample_rate: UNIT_RATE as f64,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT
                | LinearPcmFlags::IS_PACKED
                | LinearPcmFlags::IS_NON_INTERLEAVED,
            channels: 1,
        };
        let asbd = fmt.to_asbd();
        // Capture: mic element OUTPUT scope.
        unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Output,
            Element::Input,
            Some(&asbd),
        )
        .map_err(|e| format!("set capture format: {e}"))?;
        // Render: speaker element INPUT scope.
        unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(&asbd),
        )
        .map_err(|e| format!("set render format: {e}"))?;

        let (play_prod, mut play_cons) = HeapRb::<f32>::new(RENDER_CAP).split();
        let (mut cap_prod, cap_cons) = HeapRb::<f32>::new(CAPTURE_CAP).split();
        let flush = Arc::new(AtomicBool::new(false));
        let render_gen = Arc::new(AtomicU64::new(0));
        let mute = Arc::new(AtomicBool::new(false));

        // RT render: drain play ring; silence shortfall; honour pending render_clear first.
        let render_flush = flush.clone();
        let callback_render_gen = render_gen.clone();
        let render_mute = mute.clone();
        unit.set_render_callback(move |args: Args<data::NonInterleaved<f32>>| {
            let Args { mut data, .. } = args;
            if render_flush.load(Ordering::Acquire) {
                // Odd gen before clearing flush so producers wait for drain completion.
                callback_render_gen.fetch_add(1, Ordering::AcqRel);
                let should_drain = render_flush.swap(false, Ordering::AcqRel);
                let mut sink = [0.0f32; 1024];
                if should_drain {
                    while play_cons.pop_slice(&mut sink) > 0 {}
                }
                callback_render_gen.fetch_add(1, Ordering::Release);
            }
            // Mute: pop-then-zero so wall-rate drain continues and AEC ref matches speaker.
            let muted = render_mute.load(Ordering::Relaxed);
            for channel in data.channels_mut() {
                let got = play_cons.pop_slice(channel);
                if muted {
                    channel.fill(0.0);
                } else {
                    for s in channel[got..].iter_mut() {
                        *s = 0.0;
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| format!("set render callback: {e}"))?;

        // RT input: library fills AEC-cleaned mic; copy to capture ring.
        unit.set_input_callback(move |args: Args<data::NonInterleaved<f32>>| {
            let Args { mut data, .. } = args;
            for channel in data.channels_mut() {
                cap_prod.push_slice(channel); // drops samples if the helper stalls
            }
            Ok(())
        })
        .map_err(|e| format!("set input callback: {e}"))?;

        unit.initialize()
            .map_err(|e| format!("VPIO initialize: {e}"))?;

        // Disable VPIO AGC (keep AEC). VoIP AGC pumps speech and hurts Parakeet;
        // helper `capture_gain` make-up. Best-effort if OS ignores.
        // kAUVoiceIOProperty_VoiceProcessingEnableAGC = 2101, Global UInt32; 0 = off.
        const VOICE_PROCESSING_ENABLE_AGC: u32 = 2101;
        let agc_off: u32 = 0;
        let _ = unit.set_property(
            VOICE_PROCESSING_ENABLE_AGC,
            Scope::Global,
            Element::Output,
            Some(&agc_off),
        );

        unit.start().map_err(|e| format!("VPIO start: {e}"))?;

        Ok(Self {
            _unit: unit,
            capture_rate: UNIT_RATE,
            play: Arc::new(Mutex::new((
                play_prod,
                LinearResampler::new(SYNTH_RATE, UNIT_RATE),
            ))),
            cap: Arc::new(Mutex::new(cap_cons)),
            flush,
            render_gen,
            mute,
        })
    }

    /// Send+Sync capture drain while this `!Send` unit renders on the playback thread.
    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle {
            cap: self.cap.clone(),
            rate: self.capture_rate,
        }
    }

    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// VPIO owns render+capture; helper feeds TTS via [`render_push`](Self::render_push), not rodio.
    pub fn owns_render(&self) -> bool {
        true
    }

    pub fn render_push(&self, pcm_24k: &[f32]) {
        self.render_handle().push(pcm_24k);
    }

    /// Send+Sync render push while this `!Send` unit stays on the playback thread.
    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle {
            play: self.play.clone(),
            flush: self.flush.clone(),
            render_gen: self.render_gen.clone(),
            mute: self.mute.clone(),
        }
    }

    pub fn set_muted(&self, on: bool) {
        self.mute.store(on, Ordering::Relaxed);
    }

    pub fn capture_drain(&self) -> Vec<f32> {
        let mut cons = self.cap.lock().unwrap();
        let n = cons.occupied_len();
        if n == 0 {
            return Vec::new();
        }
        let mut out = vec![0.0f32; n];
        let got = cons.pop_slice(&mut out);
        out.truncate(got);
        out
    }

    pub fn render_pending(&self) -> bool {
        self.play.lock().unwrap().0.occupied_len() > 0
    }

    pub fn render_buffered(&self) -> Duration {
        self.render_handle().buffered()
    }

    /// Drop queued render on next callback (barge-in / stop).
    pub fn render_clear(&self) {
        self.flush.store(true, Ordering::Release);
        // Reset resampler: stale prev/phase would click into the next utterance.
        // Same `play` lock as push — no race.
        self.play.lock().unwrap().1 = LinearResampler::new(SYNTH_RATE, UNIT_RATE);
    }

    /// Send barge handle (`AudioUnit` is `!Send`) — same effect as render_clear off-thread.
    pub fn barge_flag(&self) -> Arc<AtomicBool> {
        self.flush.clone()
    }
}

/// Send+Sync drain of VPIO capture (concurrent with TTS render on playback thread).
#[derive(Clone)]
pub struct CaptureHandle {
    cap: Arc<Mutex<HeapCons<f32>>>,
    rate: u32,
}

impl CaptureHandle {
    pub fn capture_rate(&self) -> u32 {
        self.rate
    }

    pub fn drain(&self) -> Vec<f32> {
        let mut cons = self.cap.lock().unwrap();
        let n = cons.occupied_len();
        if n == 0 {
            return Vec::new();
        }
        let mut out = vec![0.0f32; n];
        let got = cons.pop_slice(&mut out);
        out.truncate(got);
        out
    }
}

/// Send+Sync push into VPIO render while the `!Send` unit stays on playback thread.
#[derive(Clone)]
pub struct RenderHandle {
    play: Arc<Mutex<(HeapProd<f32>, LinearResampler)>>,
    flush: Arc<AtomicBool>,
    render_gen: Arc<AtomicU64>,
    mute: Arc<AtomicBool>,
}

impl RenderHandle {
    /// Mute: wall-rate consume + zero-fill entire output (AEC ref matches speaker).
    /// Relaxed: independent of ring contents.
    pub fn set_muted(&self, on: bool) {
        self.mute.store(on, Ordering::Relaxed);
    }

    /// Push 24 kHz mono TTS (AEC reference). Resample → unit rate; full ring drops
    /// overflow (pacing bug). One feeder per request — mutex serializes but interleaved
    /// producers still corrupt order.
    pub fn push(&self, pcm_24k: &[f32]) {
        if pcm_24k.is_empty() {
            return;
        }
        // Wait out pending flush so next RT drain (for abandoned utterance) doesn't
        // also wipe samples we just wrote. Callback ticks sub-10 ms; 200 ms timeout.
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut g = loop {
            let generation = self.render_gen.load(Ordering::Acquire);
            if generation & 1 == 0 && !self.flush.load(Ordering::Acquire) {
                let g = self.play.lock().unwrap();
                // Recheck under lock: odd gen / new flush means keep waiting.
                if self.render_gen.load(Ordering::Acquire) == generation
                    && !self.flush.load(Ordering::Acquire)
                {
                    break g;
                }
                drop(g);
            }
            if Instant::now() >= deadline {
                return;
            }
            std::thread::yield_now();
        };
        let (prod, rs) = &mut *g;
        let mut scratch = Vec::with_capacity(pcm_24k.len() * 2 + 8);
        rs.process(pcm_24k, &mut scratch);
        prod.push_slice(&scratch);
    }

    /// Queued render duration (feeder lookahead occupancy).
    pub fn buffered(&self) -> Duration {
        let samples = self.play.lock().unwrap().0.occupied_len();
        Duration::from_secs_f64(samples as f64 / UNIT_RATE as f64)
    }
}
