//! macOS Voice-Processing I/O duplex audio.
//!
//! ONE `kAudioUnitSubType_VoiceProcessingIO` AudioUnit renders the speaker AND
//! captures the mic; Apple's voice processing cancels the rendered audio from the
//! capture. Because the unit owns both streams, the far-end reference and the mic
//! are already on one clock — we do no delay/drift alignment ourselves.
//!
//! Threading: the unit's render + input callbacks run on the CoreAudio realtime
//! thread. They MUST NOT block, so they talk to the helper thread through two
//! lock-free SPSC rings (ringbuf): a `play` ring (helper pushes 24 kHz→unit-rate
//! samples, the render callback drains it) and a `cap` ring (the input callback
//! pushes AEC-cleaned mic samples, the helper drains it). The producer/consumer
//! ends we keep on the helper thread sit behind a `Mutex` (helper-side only — the
//! RT thread never touches it), which is RT-safe.

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

/// Kokoro synth rate (the render input rate the caller supplies). Matches
/// `ds_tts::vocab::SAMPLE_RATE`.
const SYNTH_RATE: u32 = 24_000;

/// The rate we request from VPIO for both render and capture. VPIO is opinionated
/// (de-facto 48 kHz); we set this and treat it as negotiated. If a device forces
/// another rate `set_property(StreamFormat)` errors and `open()` fails → the
/// caller degrades to half-duplex.
const UNIT_RATE: u32 = 48_000;

/// Render ring capacity. The helper's feeder thread paces ≤100 ms chunks against a
/// ~2 s lookahead, so normal occupancy stays around 2 s; 90 s is headroom so an
/// accidental oversized push drops only its tail rather than shredding throughput.
const RENDER_CAP: usize = (UNIT_RATE * 90) as usize;

/// Capture ring: ~2 s is plenty — the helper drains it every poll tick.
const CAPTURE_CAP: usize = (UNIT_RATE * 2) as usize;

/// A live echo-cancelled duplex unit. `!Send` (holds the `AudioUnit`): open and
/// drive it on one thread.
pub struct DuplexAudio {
    /// Kept alive so the unit keeps running; dropping it stops capture+render.
    _unit: AudioUnit,
    capture_rate: u32,
    /// Helper-side producer for the render ring + the 24 kHz→unit-rate resampler.
    /// Behind `Arc` so the helper's feeder thread can push via a [`RenderHandle`]
    /// while the `!Send` `AudioUnit` stays on the playback thread.
    play: Arc<Mutex<(HeapProd<f32>, LinearResampler)>>,
    /// Helper-side consumer for the capture ring. Behind `Arc` so a separate thread
    /// (the helper's concurrent listen) can drain it via a [`CaptureHandle`] while
    /// the `!Send` `AudioUnit` stays on this thread — enabling speak+listen at once.
    cap: Arc<Mutex<HeapCons<f32>>>,
    /// Set by `render_clear()`; the render callback drains the ring on its next
    /// tick. An atomic (not a lock) so the RT thread reads it without blocking.
    flush: Arc<AtomicBool>,
    /// Even while idle, odd while the render callback is draining the ring. The
    /// callback advances it once before clearing `flush` and once after the drain,
    /// so `render_push` cannot mistake "flush claimed" for "flush completed".
    render_gen: Arc<AtomicU64>,
    /// Render-time mute: the callback keeps consuming the ring at wall rate but
    /// zero-fills the output while set. See [`set_muted`](Self::set_muted).
    mute: Arc<AtomicBool>,
}

impl DuplexAudio {
    /// Open the VPIO unit (mic capture + speaker render, AEC on). Returns an error
    /// string (for fail-quiet logging) on any CoreAudio error; the caller then
    /// falls back to the half-duplex cpal + rodio/afplay path.
    pub fn open() -> Result<Self, String> {
        // EnableIO + StreamFormat must be set before the unit is initialized.
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

        // Mono, 32-bit float, non-interleaved, packed — for both directions.
        let fmt = StreamFormat {
            sample_rate: UNIT_RATE as f64,
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT
                | LinearPcmFlags::IS_PACKED
                | LinearPcmFlags::IS_NON_INTERLEAVED,
            channels: 1,
        };
        let asbd = fmt.to_asbd();
        // Capture format = the mic element's OUTPUT scope (what we read).
        unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Output,
            Element::Input,
            Some(&asbd),
        )
        .map_err(|e| format!("set capture format: {e}"))?;
        // Render format = the speaker element's INPUT scope (what we write).
        unit.set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(&asbd),
        )
        .map_err(|e| format!("set render format: {e}"))?;

        // Lock-free rings shared with the realtime callbacks (capacities above).
        let (play_prod, mut play_cons) = HeapRb::<f32>::new(RENDER_CAP).split();
        let (mut cap_prod, cap_cons) = HeapRb::<f32>::new(CAPTURE_CAP).split();
        let flush = Arc::new(AtomicBool::new(false));
        let render_gen = Arc::new(AtomicU64::new(0));
        let mute = Arc::new(AtomicBool::new(false));

        // Render callback (RT thread): drain the play ring into the speaker; fill
        // any shortfall with silence. Honour a pending `render_clear` first.
        let render_flush = flush.clone();
        let callback_render_gen = render_gen.clone();
        let render_mute = mute.clone();
        unit.set_render_callback(move |args: Args<data::NonInterleaved<f32>>| {
            let Args { mut data, .. } = args;
            if render_flush.load(Ordering::Acquire) {
                // Mark the generation odd BEFORE clearing `flush`. A producer that
                // observes the clear therefore also observes that the drain is still
                // active and waits for the matching even generation below.
                callback_render_gen.fetch_add(1, Ordering::AcqRel);
                let should_drain = render_flush.swap(false, Ordering::AcqRel);
                let mut sink = [0.0f32; 1024];
                if should_drain {
                    while play_cons.pop_slice(&mut sink) > 0 {}
                }
                callback_render_gen.fetch_add(1, Ordering::Release);
            }
            // Mute at RENDER time, pop-then-zero: buffered audio keeps draining at
            // wall rate (unmute resumes at the playhead; audio elapsed while muted
            // is skipped), and VPIO's far-end reference IS this output buffer (see
            // the module doc), so zeroing the ENTIRE buffer keeps the reference
            // equal to the actual speaker output. RT-safe: one Relaxed atomic load,
            // no alloc/lock — mirrors the shortfall zero-fill.
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

        // Input callback (RT thread): the library calls AudioUnitRender to fill
        // `data` with the AEC-cleaned mic, then we copy it into the capture ring.
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

        // Disable VPIO's automatic gain control. We KEEP the AEC (the whole point),
        // but the AGC is VoIP-tuned and pumps/distorts speech, which hurts Parakeet
        // accuracy. The make-up gain (`capture_gain` config, applied in the helper's
        // listen path) compensates for the level the AGC was providing. Best-effort:
        // an OS that doesn't honour it just keeps AGC on.
        // kAUVoiceIOProperty_VoiceProcessingEnableAGC (AudioUnitProperties.h) = 2101,
        // a UInt32 on the Global scope; 0 = off.
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

    /// A `Send`+`Sync` handle to the echo-cancelled capture ring, so the helper's
    /// concurrent listen thread can drain the mic WHILE this (`!Send`) unit renders
    /// TTS on the playback thread.
    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle {
            cap: self.cap.clone(),
            rate: self.capture_rate,
        }
    }

    /// The negotiated capture sample rate (drain a `capture_rate()`→16 kHz
    /// resampler before Parakeet).
    pub fn capture_rate(&self) -> u32 {
        self.capture_rate
    }

    /// macOS VPIO owns BOTH render and capture on one clock, so the helper feeds
    /// TTS through [`render_push`](Self::render_push) (the AEC reference) and skips
    /// rodio. Capture-side backends (Windows/Linux) return `false` and keep rodio.
    pub fn owns_render(&self) -> bool {
        true
    }

    /// See [`RenderHandle::push`] (this delegates to a fresh handle).
    pub fn render_push(&self, pcm_24k: &[f32]) {
        self.render_handle().push(pcm_24k);
    }

    /// A `Send`+`Sync` push handle for the render ring, so the helper's feeder
    /// thread can pace committed TTS into VPIO while this (`!Send`) unit stays on
    /// the playback thread.
    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle {
            play: self.play.clone(),
            flush: self.flush.clone(),
            render_gen: self.render_gen.clone(),
            mute: self.mute.clone(),
        }
    }

    /// See [`RenderHandle::set_muted`].
    pub fn set_muted(&self, on: bool) {
        self.mute.store(on, Ordering::Relaxed);
    }

    /// Drain the echo-cancelled mono f32 captured since the last call (at
    /// `capture_rate()`). Empty when no new audio has arrived.
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

    /// Whether the render ring still holds unplayed samples (is TTS still sounding).
    pub fn render_pending(&self) -> bool {
        self.play.lock().unwrap().0.occupied_len() > 0
    }

    /// See [`RenderHandle::buffered`].
    pub fn render_buffered(&self) -> Duration {
        self.render_handle().buffered()
    }

    /// Drop queued render audio on the next callback tick (barge-in / stop).
    pub fn render_clear(&self) {
        self.flush.store(true, Ordering::Release);
        // The persistent `LinearResampler` still holds interpolation state (the
        // last sample of the abandoned utterance + fractional phase) from before
        // this barge. Left alone, the next `render_push()` would linearly blend
        // that stale last sample against the first sample of the NEW utterance,
        // injecting an audible click/pop at the barge boundary. Replacing it with
        // a fresh resampler (same rates) resets that state so playback resumes
        // clean. Guarded by the same `play` lock `render_push()` uses, so this
        // can't race a concurrent push.
        self.play.lock().unwrap().1 = LinearResampler::new(SYNTH_RATE, UNIT_RATE);
    }

    /// A `Send` barge handle (the `AudioUnit` itself is `!Send`). Another thread
    /// can store `true` to drain the render ring on the next callback — the same
    /// effect as [`render_clear`](Self::render_clear), reachable off-thread (the
    /// helper's stdin reader uses it for instant barge-in).
    pub fn barge_flag(&self) -> Arc<AtomicBool> {
        self.flush.clone()
    }
}

/// A `Send`+`Sync` drain handle for the VPIO capture ring (see
/// [`DuplexAudio::capture_handle`]). Lets the helper's listen thread read the
/// echo-cancelled mic concurrently with TTS render on the playback thread.
#[derive(Clone)]
pub struct CaptureHandle {
    cap: Arc<Mutex<HeapCons<f32>>>,
    rate: u32,
}

impl CaptureHandle {
    /// The negotiated capture sample rate (feed through a `rate`→16 kHz resampler).
    pub fn capture_rate(&self) -> u32 {
        self.rate
    }

    /// Drain the echo-cancelled mono f32 captured since the last call. Empty when
    /// no new audio has arrived.
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

/// A `Send`+`Sync` push handle for the VPIO render ring (see
/// [`DuplexAudio::render_handle`]). Lets the helper's feeder thread pace committed
/// TTS into VPIO while the `!Send` unit stays on the playback thread.
#[derive(Clone)]
pub struct RenderHandle {
    play: Arc<Mutex<(HeapProd<f32>, LinearResampler)>>,
    flush: Arc<AtomicBool>,
    render_gen: Arc<AtomicU64>,
    mute: Arc<AtomicBool>,
}

impl RenderHandle {
    /// Mute/unmute at render time: the callback keeps consuming the ring at wall
    /// rate but zero-fills the ENTIRE output buffer while set — the ring holds real
    /// audio, so unmute resumes at the playhead instantly, and audio elapsed while
    /// muted is still skipped (mute consumes speech). `Relaxed` on purpose: an
    /// independent flag with no ordering dependency on ring contents.
    pub fn set_muted(&self, on: bool) {
        self.mute.store(on, Ordering::Relaxed);
    }

    /// Push 24 kHz mono f32 TTS PCM to be rendered (and used as the AEC reference).
    /// Resamples to the unit rate and writes the play ring. Non-blocking; if the
    /// ring is full the overflow is dropped (a full ring indicates a pacing bug in
    /// the feeder, not a long reply). Exactly ONE feeding site may push per
    /// request: the `play` mutex serializes concurrent pushers, but interleaved
    /// producers would corrupt sample order.
    pub fn push(&self, pcm_24k: &[f32]) {
        if pcm_24k.is_empty() {
            return;
        }
        // If a flush requested by `render_clear()`/`barge_flag()` is still pending
        // (the render callback hasn't ticked since it was set), wait briefly for the
        // callback to service it before we add new samples. Otherwise the
        // callback's next tick would perform its unconditional full-ring drain —
        // meant to discard the ABANDONED utterance — just after we've written
        // THESE new samples, sweeping them away too (the drain can't tell old
        // samples from new ones already sitting in the ring). The render callback
        // ticks every audio quantum (sub-10ms) for as long as the unit is running,
        // so this is a short, bounded wait, not a real block; a generous timeout
        // guards against ever hanging this (non-RT) thread if the unit stalls.
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut g = loop {
            let generation = self.render_gen.load(Ordering::Acquire);
            if generation & 1 == 0 && !self.flush.load(Ordering::Acquire) {
                let g = self.play.lock().unwrap();
                // Recheck after acquiring the producer lock. If the callback claimed
                // a pending flush in the gap, its odd generation keeps us out until
                // the drain is complete; if a new clear arrived, `flush` keeps us out.
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

    /// Duration of audio currently queued ahead of the realtime render callback —
    /// the feeder thread's occupancy signal for its lookahead, instead of filling
    /// the whole ring before a live mute transition can be observed.
    pub fn buffered(&self) -> Duration {
        let samples = self.play.lock().unwrap().0.occupied_len();
        Duration::from_secs_f64(samples as f64 / UNIT_RATE as f64)
    }
}
