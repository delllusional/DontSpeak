//! FluidAudio Core ML / ANE Kokoro TTS (load-only C ABI shim).
//!
//! Own signed `libdontspeak_fluid.dylib` via [`ds_model::shim`] (peer of MLX, not the same
//! file). Rust owns G2P — shim takes validated phonemes so ORT/MLX/Fluid share chunks.
//! Packs are `ANE/<voice>.bin` with no fallback; materialize before FFI.

use std::ffi::{CString, c_char, c_void};

use ds_model::shim::PcmCb;
use libloading::{Library, Symbol};

// model_dir, compute_units (dontspeak_fluid.h).
type InitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
// phonemes, voice, speed, ctx, cb.
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
        // SAFETY: `synth` owns the lib; CString lives for the call. compute_units=0 reserved.
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
        // ANE has no voice fallback; materialize before FFI (models-tool msg if npz missing).
        crate::ane_voices::materialize(voice)?;
        let phonemes = CString::new(phonemes).map_err(|_| "phonemes contain NUL".to_string())?;
        let voice = CString::new(voice).map_err(|_| "voice contains NUL".to_string())?;
        // SAFETY: `SynthFn`; CStrings + collect_pcm pair outlive the call.
        let synth: Symbol<SynthFn> = unsafe { self.lib.get(b"ds_fluid_tts_synthesize_phonemes\0") }
            .map_err(|error| format!("ds_fluid_tts_synthesize_phonemes symbol: {error}"))?;
        ds_model::shim::collect_pcm(|ctx, callback| {
            // SAFETY: both C strings remain valid for this blocking call; `collect_pcm`
            // supplies the synchronous context/callback pair, which the shim does not retain.
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
        // SAFETY: init succeeded; shutdown at most once while lib is loaded.
        unsafe {
            if let Ok(shutdown) = self.lib.get::<ShutdownFn>(b"ds_fluid_tts_shutdown\0") {
                shutdown();
            }
        }
    }
}
