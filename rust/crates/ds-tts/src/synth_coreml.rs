//! Apple-native Kokoro TTS backend (macOS).
//!
//! `dlopen`s `libsmkokoro.dylib` — a Swift `@_cdecl` shim over FluidAudio's
//! ANE-resident Core ML Kokoro — and synthesizes **text** → 24 kHz mono f32 PCM.
//! Unlike the ONNX path ([`crate::synth::KokoroSynth`]) the Core ML chain runs its
//! own G2P internally, so this takes raw text (no phonemes, no vocab/tokenization).
//!
//! Selected when `DONTSPEAK_PROVIDER=apple-native`; the helper falls back to the ONNX
//! CPU path if the dylib or models are unavailable. The dylib is located via
//! `SMKOKORO_DYLIB_PATH` (set by the macOS app, mirroring `ORT_DYLIB_PATH`). Models
//! download on first use. See `apps/macos/SmKokoro/include/smkokoro.h`.

use std::ffi::{CString, c_char, c_void};

use ds_stt::shim::PcmCb;
use libloading::{Library, Symbol};

type InitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
// Synthesis still BLOCKS and returns its status; the PCM comes back through a borrowed callback
// (copied out by `ds_stt::shim::collect_pcm`), so there is no out-param and no `smk_free`.
type SynthFn = unsafe extern "C" fn(*const c_char, *const c_char, f32, *mut c_void, PcmCb) -> i32;
type ShutdownFn = unsafe extern "C" fn();

/// FluidAudio's guaranteed English voice — used when the requested voice has no
/// converted ANE voice pack.
const FALLBACK_VOICE: &str = "af_heart";

/// FluidAudio's Core ML Kokoro behind the C ABI. One per helper process.
pub struct CoremlKokoro {
    lib: Library,
}

impl CoremlKokoro {
    /// `dlopen` the shim and initialize the model store (downloads models on first
    /// use). Honors `SMKOKORO_DYLIB_PATH`. Errors (missing dylib, init/download
    /// failure) are returned so the helper can fall back to ONNX.
    pub fn load() -> Result<Self, String> {
        // Shared shim loader (also used by the Parakeet STT backend) — resolves
        // SMKOKORO_DYLIB_PATH + dlopens, so the two backends can't drift.
        let lib = ds_stt::shim::open()?;
        let me = CoremlKokoro { lib };
        // Pass our DontSpeak-controlled Core ML cache dir (not "" → FluidAudio's scattered
        // default) so the Kokoro model downloads under our cache folder; compute_units 0 →
        // default ANE routing.
        let rc = unsafe {
            let init: Symbol<InitFn> = me
                .lib
                .get(b"smk_init\0")
                .map_err(|e| format!("smk_init symbol: {e}"))?;
            let dir = ds_stt::shim::model_dir_arg();
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("smk_init failed (rc={rc})"));
        }
        // Absorb Core ML's one-time graph specialization here (≈1 s) with a throwaway
        // synth, so the user's FIRST real utterance is warm (~11× RTF) instead of
        // paying the cold penalty (~2.5×). Errors are non-fatal — the real call retries.
        let _ = me.synthesize_one("Ready.", FALLBACK_VOICE, 1.0);
        Ok(me)
    }

    /// Active provider label for the engine stats / `PROVIDER` line.
    pub fn provider(&self) -> &'static str {
        "CoreML-ANE"
    }

    /// Synthesize `text` → 24 kHz mono f32 PCM. `voice` is a Kokoro voice id. The ANE
    /// repo ships only `af_heart`, but any voice is materialized on demand from the
    /// LOCAL voices npz, so first use Just Works; an id with no local source falls back
    /// to `af_heart`. Empty result is returned as an empty Vec (the caller skips it).
    pub fn synthesize_text(&self, text: &str, voice: &str, speed: f32) -> Result<Vec<f32>, String> {
        // FluidAudio's neural BART G2P fallback sounds bare digit runs out as
        // garbage (heard as "X"), so expand numbers to English words first. The ANE
        // chain is English-only here, so unconditional English expansion is correct.
        // See [`crate::numbers`].
        let text = crate::numbers::expand_numbers(text);
        // Resolve to a voice whose pack is GUARANTEED resident on disk, so FluidAudio's
        // `ensureVoicePack` always hits the local file and NEVER makes a network call.
        let voice = self.resident_voice(voice);
        match self.synthesize_one(&text, &voice, speed) {
            Ok(pcm) => Ok(pcm),
            Err(e) if voice != FALLBACK_VOICE => {
                eprintln!(
                    "dontspeak/helper: coreml voice '{voice}' failed ({e}); using {FALLBACK_VOICE}"
                );
                self.synthesize_one(&text, FALLBACK_VOICE, speed)
            }
            Err(e) => Err(e),
        }
    }

    /// Map a requested voice to one whose ANE pack is already on disk — fully OFFLINE:
    ///   1. `af_heart` (ships with the model) or an already-materialized voice → as-is.
    ///   2. else extract it from the local `voices-v1.0.bin` (no download).
    ///   3. else (npz absent / unknown id) → `af_heart`, WITHOUT ever asking the shim
    ///      for the missing voice — which is what would trigger FluidAudio's network
    ///      fetch. So synthesis never makes a network call; only the explicit
    ///      `download_models { voice }` tool may go to the network (to get the npz).
    fn resident_voice(&self, voice: &str) -> String {
        if voice == FALLBACK_VOICE || crate::ane_voices::is_materialized(voice) {
            return voice.to_string();
        }
        match crate::ane_voices::materialize(voice) {
            Ok(_) => voice.to_string(),
            Err(e) => {
                eprintln!(
                    "dontspeak/helper: '{voice}' not resident and no local source ({e}); using {FALLBACK_VOICE}"
                );
                FALLBACK_VOICE.to_string()
            }
        }
    }

    /// One FFI synthesis call for an exact voice (no fallback).
    fn synthesize_one(&self, text: &str, voice: &str, speed: f32) -> Result<Vec<f32>, String> {
        let c_text = CString::new(text).map_err(|_| "text contains NUL".to_string())?;
        let c_voice = CString::new(voice).map_err(|_| "voice contains NUL".to_string())?;
        let synth: Symbol<SynthFn> = unsafe { self.lib.get(b"smk_synthesize_text\0") }
            .map_err(|e| format!("smk_synthesize_text symbol: {e}"))?;
        // The shim BORROWS the PCM to our sink, which copies it into a `Vec<f32>` while the shim
        // still owns it — so there's no ownership transfer, no `smk_free`, and no raw-pointer/len
        // guards here. The call blocks; `c_text`/`c_voice` live across it. The sample rate is
        // 24_000 for Kokoro (the pipeline assumes 24 kHz, so we don't resample); an empty/no-audio
        // result comes back as an empty Vec.
        ds_stt::shim::collect_pcm(|ctx, cb| unsafe {
            synth(c_text.as_ptr(), c_voice.as_ptr(), speed, ctx, cb)
        })
        .map_err(|rc| format!("smk_synthesize_text failed (rc={rc})"))
    }
}

impl Drop for CoremlKokoro {
    fn drop(&mut self) {
        // SAFETY: shim shutdown is idempotent; called once as the helper drops it.
        unsafe {
            if let Ok(shutdown) = self.lib.get::<ShutdownFn>(b"smk_shutdown\0") {
                shutdown();
            }
        }
    }
}
