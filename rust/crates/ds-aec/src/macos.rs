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
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Render ring capacity. The helper synthesizes a whole reply UP FRONT and Kokoro
/// runs FASTER than real time, so the producer races ahead of the real-time render
/// callback — the ring must hold an entire reply or the tail is dropped (choppy,
/// truncated playback). 90 s of mono f32 (~17 MB) covers any realistic reply; a
/// longer one degrades by dropping its tail rather than shredding throughout.
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
    play: Mutex<(HeapProd<f32>, LinearResampler)>,
    /// Helper-side consumer for the capture ring. Behind `Arc` so a separate thread
    /// (the helper's concurrent listen) can drain it via a [`CaptureHandle`] while
    /// the `!Send` `AudioUnit` stays on this thread — enabling speak+listen at once.
    cap: Arc<Mutex<HeapCons<f32>>>,
    /// Set by `render_clear()`; the render callback drains the ring on its next
    /// tick. An atomic (not a lock) so the RT thread reads it without blocking.
    flush: Arc<AtomicBool>,
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

        // Render callback (RT thread): drain the play ring into the speaker; fill
        // any shortfall with silence. Honour a pending `render_clear` first.
        let render_flush = flush.clone();
        unit.set_render_callback(move |args: Args<data::NonInterleaved<f32>>| {
            let Args { mut data, .. } = args;
            if render_flush.swap(false, Ordering::AcqRel) {
                let mut sink = [0.0f32; 1024];
                while play_cons.pop_slice(&mut sink) > 0 {}
            }
            for channel in data.channels_mut() {
                let got = play_cons.pop_slice(channel);
                for s in channel[got..].iter_mut() {
                    *s = 0.0;
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
            play: Mutex::new((play_prod, LinearResampler::new(SYNTH_RATE, UNIT_RATE))),
            cap: Arc::new(Mutex::new(cap_cons)),
            flush,
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

    /// Push 24 kHz mono f32 TTS PCM to be rendered (and used as the AEC reference).
    /// Resamples to the unit rate and writes the play ring. Non-blocking; if the
    /// ring is full the overflow is dropped (only for a reply longer than the
    /// 90 s render ring).
    pub fn render_push(&self, pcm_24k: &[f32]) {
        if pcm_24k.is_empty() {
            return;
        }
        let mut g = self.play.lock().unwrap();
        let (prod, rs) = &mut *g;
        let mut scratch = Vec::with_capacity(pcm_24k.len() * 2 + 8);
        rs.process(pcm_24k, &mut scratch);
        prod.push_slice(&scratch);
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

    /// Drop queued render audio on the next callback tick (barge-in / stop).
    pub fn render_clear(&self) {
        self.flush.store(true, Ordering::Release);
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
