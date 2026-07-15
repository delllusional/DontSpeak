//! One-shot (non-serve) mode: load the backend once, synth + play, then exit.
//! Owns the [`Backend`] enum and its loaders ([`load_synth`], [`load_backend`]),
//! which the warm serve loop also uses.

use ds_tts::g2p;
use ds_tts::play::AudioPlayer;
use ds_tts::synth::KokoroSynth;

use crate::prepare::prepare_audio;

/// Ensure the assets, point ort at the dylib, and build the session ONCE.
fn load_synth() -> Result<KokoroSynth, String> {
    let model_path =
        ds_model::model_path(ds_model::KOKORO_ONNX_FILE).ok_or("cannot resolve model_dir()")?;
    let voices_path =
        ds_model::model_path(ds_model::KOKORO_VOICES_FILE).ok_or("cannot resolve model_dir()")?;
    // Do NOT download here — enabling TTS must use an already-downloaded model and
    // FAIL (so the UI shows a red dot) when it's missing, never auto-fetch it.
    if !ds_model::is_kokoro_present() {
        return Err("kokoro model not downloaded".into());
    }
    // Pick the ONNX runtime via the SHARED GPU-aware bootstrap (the SAME one Parakeet
    // STT uses, so both engines share one ort runtime): on Windows, the CUDA GPU
    // onnxruntime when CUDA is the preference AND its (separately fetched) GPU runtime is
    // present, else the version-gated CPU dylib. `ensure_ort_dylib_gpu` sets ORT_DYLIB_PATH
    // and the CUDA loader search path itself. synth.rs then registers the CUDA EP,
    // CPU-fallback on fail.
    let want_gpu = ds_config::provider_pref_wants_gpu(
        &std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into()),
    );
    ds_model::ensure_ort_dylib_gpu(want_gpu)?;
    let model_bytes = ds_model::read_model_file(&model_path)?;
    let voices_bytes = ds_model::read_model_file(&voices_path)?;
    KokoroSynth::load(&model_bytes, &voices_bytes)
}

/// The active TTS backend. ONNX Kokoro ([`KokoroSynth`]) is the default + fallback;
/// `apple_native` (macOS) routes to FluidAudio's Core ML / ANE Kokoro. Both now consume the
/// SAME validated `KokoroPhonemeChunk`s from the shared Rust frontend — the Apple side no
/// longer takes raw text and runs a G2P of its own.
pub(crate) enum Backend {
    Ort(KokoroSynth),
    #[cfg(target_os = "macos")]
    Coreml(ds_tts::synth_coreml::KokoroCoremlTts),
}

impl Backend {
    /// The REALIZED provider for the engine stats / `PROVIDER` line (CPU/CoreML/CUDA for ONNX,
    /// CoreML-ANE for the apple-native backend) — the shared `RealizedProvider` type.
    pub(crate) fn provider(&self) -> ds_config::RealizedProvider {
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
            match ds_tts::synth_coreml::KokoroCoremlTts::load() {
                Ok(c) => return Ok(Backend::Coreml(c)),
                Err(e) => log::warn!(
                    target: "helper",
                    "ANE (FluidAudio) TTS unavailable ({e}); falling back to ONNX"
                ),
            }
        }
    }
    Ok(Backend::Ort(load_synth()?))
}

/// One-shot: offer each validated phoneme batch to `AudioPlayer` as soon as it is ready.
/// Non-macOS playback starts incrementally; macOS accumulates for reliable `afplay` teardown.
pub(crate) fn run(text: &str, voice: &str, rate: f32) -> Result<(), String> {
    // Keep the cold path aligned with `serve`: both backends consume the same normalized,
    // model-bounded Rust phoneme batches.
    let phoneme_batches = g2p::phoneme_batches_for(text, voice)?;
    // Nothing speakable (image-only / emoji-only / punctuation-only). A successful no-op, not a
    // failure — returning Err here made `ds-helper "🎉"` exit nonzero. Bail before opening a
    // device we have nothing to play through.
    if phoneme_batches.is_empty() {
        return Ok(());
    }
    // Open the player only after the first full batch succeeds. This keeps a failure in the
    // first batch silent; the platform player decides whether enqueue starts playback or safely
    // accumulates until `wait`.
    let mut player: Option<AudioPlayer> = None;
    let mut commit = |audio: crate::prepare::PreparedAudio| -> Result<(), String> {
        if player.is_none() {
            player = Some(AudioPlayer::open()?);
        }
        let player = player.as_ref().expect("opened above");
        player.enqueue(audio.pcm);
        Ok(())
    };
    match load_backend()? {
        Backend::Ort(mut synth) => prepare_audio(
            &phoneme_batches,
            || false,
            |batch| synth.synthesize(batch.as_str(), voice, rate),
            &mut commit,
        )?,
        // Core ML consumes the same Rust-produced IPA batches as ONNX.
        #[cfg(target_os = "macos")]
        Backend::Coreml(c) => prepare_audio(
            &phoneme_batches,
            || false,
            |batch| c.synthesize_phonemes(batch.as_str(), voice, rate),
            &mut commit,
        )?,
    };
    if let Some(player) = player {
        player.wait();
    }
    Ok(())
}
