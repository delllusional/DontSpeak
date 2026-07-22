//! Apple-Silicon MLX TTS through DontSpeak's load-only C ABI shim.

use std::ffi::{CString, c_char, c_void};

use ds_model::mlx_shim::PcmCb;
use libloading::{Library, Symbol};

type InitFn = unsafe extern "C" fn(*const c_char, *const c_char) -> i32;
type SynthFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    f32,
    *mut c_void,
    PcmCb,
) -> i32;
type ShutdownFn = unsafe extern "C" fn();

const KOKORO_FALLBACK_VOICE: &str = "af_heart";

pub struct MlxTts {
    lib: Library,
    model: ds_config::TtsModel,
    initialized: bool,
}

impl MlxTts {
    pub fn load(model: ds_config::TtsModel) -> Result<Self, String> {
        let dir = ds_model::mlx_shim::tts_model_dir_arg(model);
        let lib = ds_model::mlx_shim::open()?;
        let mut synth = Self {
            lib,
            model,
            initialized: false,
        };
        let model_arg = CString::new(model.as_str()).expect("model IDs contain no NUL");
        // SAFETY: the shim library remains owned by `synth`, the symbol has the declared
        // C ABI, and both CString pointers remain valid for the duration of the call.
        let rc = unsafe {
            let init: Symbol<InitFn> = synth
                .lib
                .get(b"ds_mlx_tts_init\0")
                .map_err(|error| format!("ds_mlx_tts_init symbol: {error}"))?;
            init(model_arg.as_ptr(), dir.as_ptr())
        };
        if rc != 0 {
            return Err(format!("ds_mlx_tts_init failed (rc={rc})"));
        }
        synth.initialized = true;
        Ok(synth)
    }

    pub fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Mlx
    }

    pub fn synthesize(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
    ) -> Result<Vec<f32>, String> {
        match self.synthesize_one(text, voice, language, speed) {
            Ok(pcm) => Ok(pcm),
            Err(error)
                if self.model == ds_config::TtsModel::Kokoro && voice != KOKORO_FALLBACK_VOICE =>
            {
                log::warn!(
                    "MLX Kokoro voice '{voice}' failed ({error}); using {KOKORO_FALLBACK_VOICE}"
                );
                self.synthesize_one(text, KOKORO_FALLBACK_VOICE, language, speed)
            }
            Err(error) => Err(error),
        }
    }

    fn synthesize_one(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
    ) -> Result<Vec<f32>, String> {
        let text = CString::new(text).map_err(|_| "text contains NUL".to_string())?;
        // OmniVoice preset ids resolve to their style instruct through the ONE table
        // (crate::omnivoice::OMNIVOICE_PRESETS) before crossing the FFI.
        let voice = if self.model == ds_config::TtsModel::OmniVoice {
            crate::omnivoice::mlx_voice_arg(voice)
        } else {
            voice
        };
        let voice = CString::new(voice).map_err(|_| "voice contains NUL".to_string())?;
        let language = self.model.descriptor().runtime_language(language);
        let language = CString::new(language).map_err(|_| "language contains NUL".to_string())?;
        // SAFETY: `self.lib` remains loaded while the symbol is used and the shim exports
        // this name with the `SynthFn` ABI.
        let synth: Symbol<SynthFn> = unsafe { self.lib.get(b"ds_mlx_tts_synthesize\0") }
            .map_err(|error| format!("ds_mlx_tts_synthesize symbol: {error}"))?;
        ds_model::mlx_shim::collect_pcm(|ctx, callback| {
            // SAFETY: the CStrings outlive this synchronous call; `collect_pcm` supplies a
            // matching context/callback pair that remains valid until the call returns.
            unsafe {
                synth(
                    text.as_ptr(),
                    voice.as_ptr(),
                    language.as_ptr(),
                    speed,
                    ctx,
                    callback,
                )
            }
        })
        .map_err(|rc| format!("ds_mlx_tts_synthesize failed (rc={rc})"))
    }
}

impl Drop for MlxTts {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        // SAFETY: initialization succeeded, the owned library is still loaded, and Drop
        // invokes the shim's no-argument shutdown function at most once for this backend.
        unsafe {
            if let Ok(shutdown) = self.lib.get::<ShutdownFn>(b"ds_mlx_tts_shutdown\0") {
                shutdown();
            }
        }
    }
}
