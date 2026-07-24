//! macOS FluidAudio (Core ML / ANE) Kokoro TTS through DontSpeak's load-only C ABI shim.
//!
//! Loads its own signed `libdontspeak_fluid.dylib` through [`ds_model::shim`], a peer of the
//! MLX backend's dylib rather than the same file. Rust owns G2P -- the shim takes validated
//! Kokoro phonemes, never text -- so ORT/MLX/Fluid Kokoro render from identical chunks. The ANE
//! chain reads voice packs as `ANE/<voice>.bin` and throws (no `af_heart` fallback) when one is
//! absent, so the requested pack is materialized on demand before the FFI call.

use std::ffi::{CString, c_char, c_void};

use ds_model::shim::PcmCb;
use libloading::{Library, Symbol};

// model_dir, compute_units -- mirrors dontspeak_fluid.h.
type InitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
// phonemes, voice, speed, ctx, cb -- mirrors dontspeak_mlx.h.
type SynthFn = unsafe extern "C" fn(*const c_char, *const c_char, f32, *mut c_void, PcmCb) -> i32;
type ShutdownFn = unsafe extern "C" fn();

pub struct FluidTts {
    lib: Library,
    initialized: bool,
}

impl FluidTts {
    pub fn load() -> Result<Self, String> {
        let dir = ds_model::shim::fluid_kokoro_dir_arg();
        let lib = ds_model::shim::open(ds_model::shim::Shim::Fluid)?;
        let mut synth = Self {
            lib,
            initialized: false,
        };
        // SAFETY: the shim library remains owned by `synth`, the symbol has the declared C ABI,
        // and the CString pointer stays valid for the duration of the call. `compute_units` is
        // ABI-reserved (0); the shim pins the recommended ANE preset.
        let rc = unsafe {
            let init: Symbol<InitFn> = synth
                .lib
                .get(b"ds_fluid_tts_init\0")
                .map_err(|error| format!("ds_fluid_tts_init symbol: {error}"))?;
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("ds_fluid_tts_init failed (rc={rc})"));
        }
        synth.initialized = true;
        Ok(synth)
    }

    pub fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Fluid
    }

    pub fn synthesize(&self, phonemes: &str, voice: &str, speed: f32) -> Result<Vec<f32>, String> {
        // Materialize the requested pack first: the repo ships only `af_heart`, and the ANE
        // chain has no default-voice fallback. Errors with the `models`-tool message when the
        // shared voices npz is absent.
        crate::ane_voices::materialize(voice)?;
        let phonemes = CString::new(phonemes).map_err(|_| "phonemes contain NUL".to_string())?;
        let voice = CString::new(voice).map_err(|_| "voice contains NUL".to_string())?;
        // SAFETY: `self.lib` stays loaded while the symbol is used; the shim exports this name
        // with the `SynthFn` ABI; the CStrings outlive the synchronous call.
        let synth: Symbol<SynthFn> = unsafe { self.lib.get(b"ds_fluid_tts_synthesize_phonemes\0") }
            .map_err(|error| format!("ds_fluid_tts_synthesize_phonemes symbol: {error}"))?;
        ds_model::shim::collect_pcm(|ctx, callback| {
            // SAFETY: `collect_pcm` supplies a matching context/callback pair valid until the
            // call returns; the CStrings above outlive this synchronous call.
            unsafe { synth(phonemes.as_ptr(), voice.as_ptr(), speed, ctx, callback) }
        })
        .map_err(|rc| format!("ds_fluid_tts_synthesize_phonemes failed (rc={rc})"))
    }
}

impl Drop for FluidTts {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        // SAFETY: initialization succeeded, the owned library is still loaded, and Drop invokes
        // the shim's no-argument shutdown function at most once for this backend.
        unsafe {
            if let Ok(shutdown) = self.lib.get::<ShutdownFn>(b"ds_fluid_tts_shutdown\0") {
                shutdown();
            }
        }
    }
}
