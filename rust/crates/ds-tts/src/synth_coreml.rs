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

use std::ffi::{CString, c_char};

use libloading::{Library, Symbol};

type InitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
type SynthFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    f32,
    *mut *mut f32,
    *mut usize,
    *mut i32,
) -> i32;
type FreeFn = unsafe extern "C" fn(*mut f32);
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
        let mut out_pcm: *mut f32 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut out_sr: i32 = 0;
        // SAFETY CONTRACT (ptr, len, ownership):
        //   • All in-pointers (`c_text`, `c_voice`, the three `&mut` out-slots) are
        //     valid for the duration of this call; the shim writes the out-slots only.
        //   • On rc==0 with out_len>0 the shim hands back `out_pcm`, a buffer of
        //     EXACTLY `out_len` contiguous f32 it allocated; OWNERSHIP transfers to us,
        //     so we copy it out and MUST free it via the matching `smk_free` (the same
        //     allocator) — never `Vec::from_raw_parts`/`Box`, never double-free.
        //   • On rc!=0, OR a null `out_pcm`, OR out_len==0 the buffer is NOT valid /
        //     not owned: we must neither read a slice from it NOR free it.
        let rc = unsafe {
            let synth: Symbol<SynthFn> = self
                .lib
                .get(b"smk_synthesize_text\0")
                .map_err(|e| format!("smk_synthesize_text symbol: {e}"))?;
            synth(
                c_text.as_ptr(),
                c_voice.as_ptr(),
                speed,
                &mut out_pcm,
                &mut out_len,
                &mut out_sr,
            )
        };
        // Defensive guards BEFORE building any slice or freeing (a bogus buffer must
        // never be read or freed). Order matters: bail on a non-success rc first.
        if rc != 0 {
            return Err(format!("smk_synthesize_text failed (rc={rc})"));
        }
        // A null pointer is never a valid buffer. With len==0 it's the benign
        // "no audio" case (return empty, nothing to free); with len>0 the shim
        // contradicted itself (len>0 but no buffer) — refuse rather than deref null.
        if out_pcm.is_null() {
            if out_len == 0 {
                return Ok(Vec::new());
            }
            return Err(format!(
                "smk_synthesize_text returned null buffer with len={out_len}"
            ));
        }
        // Non-null but empty: nothing to copy; do NOT free (we never read/own it).
        if out_len == 0 {
            return Ok(Vec::new());
        }
        // Sanity-bound `out_len` against an absurd max so a garbage length can't make
        // `from_raw_parts` span the whole address space (UB) before we ever read it.
        // 24 kHz mono f32 → ~96 KB/s; 1 GiB of samples is ~3 h of audio, far past any
        // single utterance, so anything beyond this is a corrupt length, not real PCM.
        const MAX_SAMPLES: usize = 256 * 1024 * 1024; // 256 Mi f32 = 1 GiB
        if out_len > MAX_SAMPLES {
            // An implausible length means the out-slots are corrupt, so `out_pcm` may
            // be garbage too. Deliberately DO NOT free it: freeing a bogus pointer is
            // UB / a crash, whereas leaking on this should-never-happen corrupt path is
            // harmless. Refuse to read it either way.
            return Err(format!(
                "smk_synthesize_text returned implausible len={out_len}"
            ));
        }
        // Copy out, then free the shim-owned buffer. (out_sr is 24_000 for Kokoro;
        // the rest of the pipeline assumes 24 kHz, so we don't resample.)
        // SAFETY: out_pcm is non-null, out_len is in (0, MAX_SAMPLES] f32, and the
        // shim guarantees that many contiguous, initialized f32 on rc==0 (see contract
        // above); the buffer outlives this read since we free it only afterwards.
        let pcm = unsafe { std::slice::from_raw_parts(out_pcm, out_len) }.to_vec();
        unsafe {
            // SAFETY: allocator-matched free of the shim-owned buffer, exactly once.
            if let Ok(free) = self.lib.get::<FreeFn>(b"smk_free\0") {
                free(out_pcm);
            }
        }
        Ok(pcm)
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
