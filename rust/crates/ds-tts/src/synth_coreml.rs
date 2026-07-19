//! Apple-native Kokoro TTS (macOS): `dlopen` `libdskokoro.dylib` (FluidAudio ANE
//! Core ML) → 24 kHz mono f32. Same [`crate::g2p`] chunks as ONNX (no platform
//! pronunciation drift).
//!
//! `DONTSPEAK_PROVIDER=apple-native`; fallback ONNX CPU if dylib/models missing.
//! Dylib via `DSKOKORO_DYLIB_PATH`. See `apps/macos/DsKokoro/include/dskokoro.h`.

use std::ffi::{CString, c_char, c_void};

use ds_model::shim::PcmCb;
use libloading::{Library, Symbol};

type InitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
// Synthesis still BLOCKS and returns its status; the PCM comes back through a borrowed callback
// (copied out by `ds_model::shim::collect_pcm`), so there is no out-param and no `dsk_free`.
type SynthFn = unsafe extern "C" fn(*const c_char, *const c_char, f32, *mut c_void, PcmCb) -> i32;
type ShutdownFn = unsafe extern "C" fn();

/// FluidAudio's guaranteed English voice — used when the requested voice has no
/// converted ANE voice pack.
const FALLBACK_VOICE: &str = "af_heart";

/// FluidAudio's Core ML Kokoro behind the C ABI. One per helper process.
pub struct KokoroCoremlTts {
    lib: Library,
    /// Set only after `dsk_init` returns success — gates `Drop` so a `load()` that
    /// fails partway through never calls `dsk_shutdown` on a store that was never
    /// (successfully) initialized.
    initialized: bool,
}

impl KokoroCoremlTts {
    /// `dlopen` the shim and initialize it from DontSpeak's pre-populated model cache.
    /// Honors `DSKOKORO_DYLIB_PATH`. Errors (missing dylib, model gap, or initialization
    /// failure) are returned so the helper can fall back to ONNX.
    pub fn load() -> Result<Self, String> {
        // Shared shim loader (also used by the Parakeet STT backend) — resolves
        // DSKOKORO_DYLIB_PATH + dlopens, so the two backends can't drift.
        let lib = ds_model::shim::open()?;
        let mut me = KokoroCoremlTts {
            lib,
            initialized: false,
        };
        // Pass our DontSpeak-controlled, pre-populated Core ML cache dir (not "" →
        // FluidAudio's scattered default). The shim is offline-only, so a cache gap fails
        // instead of downloading here; compute_units 0 → default ANE routing.
        // SAFETY: `dsk_init` is looked up by NUL-terminated name from the app-signed shim
        // and has exactly `InitFn`'s signature (dskokoro.h); the Symbol borrows `me.lib`,
        // so it can't outlive the dylib, and `dir` is a live CString across the blocking
        // call.
        let rc = unsafe {
            let init: Symbol<InitFn> = me
                .lib
                .get(b"dsk_init\0")
                .map_err(|e| format!("dsk_init symbol: {e}"))?;
            let dir = ds_model::shim::model_dir_arg();
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("dsk_init failed (rc={rc})"));
        }
        me.initialized = true;
        // Absorb Core ML's one-time graph specialization here (≈1 s) with a throwaway
        // synth, so the user's FIRST real utterance is warm (~11× RTF) instead of
        // paying the cold penalty (~2.5×). Errors are non-fatal — the real call retries.
        if let Ok(ready) = crate::g2p::phoneme_batches_for("Ready.", FALLBACK_VOICE)
            && let Some(ready) = ready.first()
        {
            let _ = me.synthesize_one(ready.as_str(), FALLBACK_VOICE, 1.0);
        }
        Ok(me)
    }

    /// The REALIZED provider for the engine stats / `PROVIDER` line — the shared type.
    pub fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::CoreMlAne
    }

    /// Synthesize one bounded IPA `phonemes` batch → 24 kHz mono f32 PCM. `voice` is a
    /// Kokoro voice id. The caller owns DontSpeak's 509-phoneme-character bound through
    /// [`crate::g2p::phoneme_batches_for`]. The ANE
    /// repo ships only `af_heart`, but any voice is materialized on demand from the
    /// LOCAL voices npz, so first use Just Works; an id with no local source falls back
    /// to `af_heart`. An empty result is returned as an empty Vec; the helper treats empty
    /// PCM from a nonempty phoneme chunk as a terminal synthesis failure (see ds-helper's
    /// transactional prepare module).
    pub fn synthesize_phonemes(
        &self,
        phonemes: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Vec<f32>, String> {
        // Resolve to a voice whose pack is GUARANTEED resident on disk, so FluidAudio's
        // `ensureVoicePack` always hits the local file and NEVER makes a network call.
        let voice = self.resident_voice(voice);
        match self.synthesize_one(phonemes, &voice, speed) {
            Ok(pcm) => Ok(pcm),
            Err(e) if voice != FALLBACK_VOICE => {
                eprintln!(
                    "dontspeak/helper: coreml voice '{voice}' failed ({e}); using {FALLBACK_VOICE}"
                );
                self.synthesize_one(phonemes, FALLBACK_VOICE, speed)
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
    fn synthesize_one(&self, phonemes: &str, voice: &str, speed: f32) -> Result<Vec<f32>, String> {
        let c_phonemes = CString::new(phonemes).map_err(|_| "phonemes contain NUL".to_string())?;
        let c_voice = CString::new(voice).map_err(|_| "voice contains NUL".to_string())?;
        // SAFETY: `dsk_synthesize_phonemes` in the app-signed shim has exactly `SynthFn`'s
        // signature (dskokoro.h); the returned Symbol borrows `self.lib`, so it can't
        // outlive the dylib.
        let synth: Symbol<SynthFn> = unsafe { self.lib.get(b"dsk_synthesize_phonemes\0") }
            .map_err(|e| format!("dsk_synthesize_phonemes symbol: {e}"))?;
        // The shim BORROWS the PCM to our sink, which copies it into a `Vec<f32>` while the shim
        // still owns it — so there's no ownership transfer, no `dsk_free`, and no raw-pointer/len
        // guards here. The call blocks; the C strings live across it. The sample rate is
        // 24_000 for Kokoro (the pipeline assumes 24 kHz, so we don't resample); an empty/no-audio
        // result comes back as an empty Vec.
        // SAFETY: `c_phonemes`/`c_voice` are live NUL-terminated CStrings across the blocking
        // call, and `ctx`/`cb` are the borrowed-result pair `collect_pcm` supplies (its
        // own stack slot + sink, fired synchronously per dskokoro.h's callback contract).
        ds_model::shim::collect_pcm(|ctx, cb| unsafe {
            synth(c_phonemes.as_ptr(), c_voice.as_ptr(), speed, ctx, cb)
        })
        .map_err(|rc| format!("dsk_synthesize_phonemes failed (rc={rc})"))
    }
}

impl Drop for KokoroCoremlTts {
    fn drop(&mut self) {
        // Gated on `initialized`: calling dsk_shutdown after a failed/incomplete
        // dsk_init is UB, and `load()` returns this struct via `Err(...)` early-outs
        // that still run Drop on the partially-built value.
        if !self.initialized {
            return;
        }
        // SAFETY: shim shutdown is idempotent; called once as the helper drops it.
        unsafe {
            if let Ok(shutdown) = self.lib.get::<ShutdownFn>(b"dsk_shutdown\0") {
                shutdown();
            }
        }
    }
}
