//! One-shot (non-serve) mode: load the backend once, synth + play, then exit.
//! Owns the [`Backend`] enum and its loaders ([`load_synth`], [`load_backend`]),
//! which the warm serve loop also uses.

use ds_tts::g2p;
use ds_tts::play::AudioPlayer;
use ds_tts::synth::KokoroSynth;

use crate::prepare::prepare_audio;

/// Point ort at the dylib and build the session once. Does not download.
fn load_synth() -> Result<KokoroSynth, String> {
    let model_path =
        ds_model::model_path(ds_model::KOKORO_ONNX_FILE).ok_or("cannot resolve model_dir()")?;
    let voices_path =
        ds_model::model_path(ds_model::KOKORO_VOICES_FILE).ok_or("cannot resolve model_dir()")?;
    // Never auto-fetch here — missing model must FAIL (red UI), not download on enable.
    if !ds_model::is_kokoro_present() {
        return Err("kokoro model not downloaded".into());
    }
    // Shared GPU-aware bootstrap with Parakeet STT (one ort runtime in-process).
    let want_gpu = ds_config::provider_pref_wants_gpu(
        &std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into()),
    );
    ds_model::ensure_ort_dylib_gpu(want_gpu)?;
    let model_bytes = ds_model::read_model_file(&model_path)?;
    let voices_bytes = ds_model::read_model_file(&voices_path)?;
    KokoroSynth::load(&model_bytes, &voices_bytes)
}

/// Active TTS backend. ONNX default/fallback; macOS `apple_native` → FluidAudio Core ML.
/// Both take the same validated Rust frontend phoneme chunks (no Apple-side G2P).
pub(crate) enum Backend {
    Ort(KokoroSynth),
    #[cfg(target_os = "macos")]
    Coreml(ds_tts::synth_coreml::KokoroCoremlTts),
}

impl Backend {
    /// Realized EP for engine stats / `PROVIDER` line.
    pub(crate) fn provider(&self) -> ds_config::RealizedProvider {
        match self {
            Backend::Ort(s) => s.provider(),
            #[cfg(target_os = "macos")]
            Backend::Coreml(c) => c.provider(),
        }
    }
}

/// From `DONTSPEAK_PROVIDER`. On macOS, `ane`/`auto` try FluidAudio Core ML first; fall back to ONNX.
pub(crate) fn load_backend() -> Result<Backend, String> {
    #[cfg(target_os = "macos")]
    {
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

/// One-shot: commit each validated batch to `AudioPlayer` (see `ds_tts::play`).
pub(crate) fn run(text: &str, voice: &str, rate: f32) -> Result<(), String> {
    // Same normalized phoneme batches as `serve` (cold path aligned with warm).
    let phoneme_batches = g2p::phoneme_batches_for(text, voice)?;
    // Nothing speakable → successful no-op (not Err — `ds-helper "🎉"` must exit 0).
    if phoneme_batches.is_empty() {
        return Ok(());
    }
    // Open the player only after the first full batch succeeds (silent first-batch failure).
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
