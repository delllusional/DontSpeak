//! One-shot (non-serve) mode: load the backend once, synth + play, then exit.
//! Owns the [`Backend`] enum and its loaders ([`load_synth`], [`load_backend`]),
//! which the warm serve loop also uses.

use ds_tts::batch::stream_batches;
use ds_tts::g2p;
use ds_tts::play::AudioPlayer;
use ds_tts::synth::KokoroSynth;

/// Ensure the assets, point ort at the dylib, and build the session ONCE.
fn load_synth() -> Result<KokoroSynth, String> {
    let model_path =
        ds_model::model_path(ds_model::KOKORO_ONNX_FILE).ok_or("cannot resolve model_dir()")?;
    let voices_path =
        ds_model::model_path(ds_model::KOKORO_VOICES_FILE).ok_or("cannot resolve model_dir()")?;
    // Do NOT download here — enabling TTS must use an already-downloaded model and
    // FAIL (so the UI shows a red dot) when it's missing, never auto-fetch it.
    if !ds_model::kokoro_present() {
        return Err("kokoro model not downloaded".into());
    }
    // Pick the ONNX runtime via the SHARED GPU-aware bootstrap (the SAME one Parakeet
    // STT uses, so both engines share one ort runtime): on Windows, the CUDA GPU
    // onnxruntime when CUDA is the preference AND its (separately fetched) GPU runtime is
    // present, else the version-gated CPU dylib. `ensure_ort_dylib_gpu` sets ORT_DYLIB_PATH
    // and the CUDA loader search path itself. synth.rs then registers the CUDA EP,
    // CPU-fallback on fail.
    let want_gpu = {
        let pref = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into());
        pref.eq_ignore_ascii_case("cuda") || pref.eq_ignore_ascii_case("auto")
    };
    ds_model::ensure_ort_dylib_gpu(want_gpu)?;
    let model_bytes = std::fs::read(&model_path).map_err(|e| format!("read model: {e}"))?;
    let voices_bytes = std::fs::read(&voices_path).map_err(|e| format!("read voices: {e}"))?;
    KokoroSynth::load(&model_bytes, &voices_bytes)
}

/// The active TTS backend. ONNX Kokoro ([`KokoroSynth`]) is the default + fallback;
/// `apple_native` (macOS) routes to FluidAudio's Core ML / ANE Kokoro, which takes
/// raw text and runs its own G2P.
pub(crate) enum Backend {
    Ort(KokoroSynth),
    #[cfg(target_os = "macos")]
    Coreml(ds_tts::synth_coreml::CoremlKokoro),
}

impl Backend {
    /// Provider label for the engine stats / `PROVIDER` line ("CPU"/"CoreML"/"CUDA"
    /// for ONNX, "CoreML-ANE" for the apple-native backend).
    pub(crate) fn provider(&self) -> &'static str {
        match self {
            Backend::Ort(s) => s.provider(),
            #[cfg(target_os = "macos")]
            Backend::Coreml(c) => c.provider(),
        }
    }
}

/// Pick the backend from `DONTSPEAK_PROVIDER`. On macOS, `ane` loads the native
/// FluidAudio Core ML / ANE Kokoro shim; if that's unavailable (no dylib, models missing,
/// init failure) we log and fall back to the ONNX path so TTS still works.
pub(crate) fn load_backend() -> Result<Backend, String> {
    #[cfg(target_os = "macos")]
    {
        // `ane` AND `auto` prefer the FluidAudio Core ML / ANE backend on macOS — the top
        // rung of the shared provider ladder. If it's unavailable (no dylib, models missing,
        // init failure) we log and fall back to the ONNX path so TTS still works.
        let pref = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_default();
        if pref.eq_ignore_ascii_case("ane") || pref.eq_ignore_ascii_case("auto") {
            match ds_tts::synth_coreml::CoremlKokoro::load() {
                Ok(c) => return Ok(Backend::Coreml(c)),
                Err(e) => eprintln!(
                    "dontspeak/helper: ANE (FluidAudio) TTS unavailable ({e}); falling back to ONNX"
                ),
            }
        }
    }
    Ok(Backend::Ort(load_synth()?))
}

/// One-shot: load + stream synth/playback through `AudioPlayer`.
pub(crate) fn run(text: &str, voice: &str, rate: f32) -> Result<(), String> {
    let player = AudioPlayer::open()?;
    match load_backend()? {
        Backend::Ort(mut synth) => {
            let mut synth_err: Option<String> = None;
            // Phonemize the whole text once, then synth ramped batches (gapless; see
            // the serve loop). `synthesize` re-batches at the 510 cap internally, so
            // passing a ≤510 batch is a single forward pass.
            let phonemes = g2p::phonemize_for(text, voice);
            for batch in stream_batches(&phonemes) {
                match synth.synthesize(&batch, voice, rate) {
                    Ok(pcm) if pcm.is_empty() => continue,
                    Ok(pcm) => player.enqueue(pcm),
                    Err(e) => {
                        synth_err = Some(e);
                        break;
                    }
                }
            }
            player.wait();
            match synth_err {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
        // FluidAudio returns the whole utterance at once (its Core ML chain
        // phonemizes internally), so there's no per-batch streaming here.
        #[cfg(target_os = "macos")]
        Backend::Coreml(c) => {
            let pcm = c.synthesize_text(text, voice, rate)?;
            if !pcm.is_empty() {
                player.enqueue(pcm);
            }
            player.wait();
            Ok(())
        }
    }
}
