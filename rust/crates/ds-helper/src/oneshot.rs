//! One-shot synthesis and the backend factory shared with warm serve mode.

use ds_tts::chatterbox::synth::ChatterboxSynth;
use ds_tts::g2p;
use ds_tts::omnivoice::OmniVoiceSynth;
use ds_tts::play::AudioPlayer;
use ds_tts::qwen::QwenSynth;
use ds_tts::synth::KokoroSynth;

use crate::prepare::{PrepareOutcome, PreparedAudio, prepare_audio};

pub(crate) fn tts_model() -> ds_config::TtsModel {
    model_from(std::env::var("DONTSPEAK_TTS_MODEL").ok().as_deref())
}

fn model_from(token: Option<&str>) -> ds_config::TtsModel {
    token
        .and_then(ds_config::TtsModel::parse)
        .unwrap_or(ds_config::TtsModel::Kokoro)
}

fn load_kokoro() -> Result<KokoroSynth, String> {
    let model_path =
        ds_model::model_path(ds_model::KOKORO_ONNX_FILE).ok_or("cannot resolve model_dir()")?;
    let voices_path =
        ds_model::model_path(ds_model::KOKORO_VOICES_FILE).ok_or("cannot resolve model_dir()")?;
    if !ds_model::is_tts_model_present(ds_config::TtsModel::Kokoro, false) {
        return Err("kokoro model not downloaded".into());
    }
    ensure_ort(ds_config::TtsModel::Kokoro)?;
    let model_bytes = ds_model::read_model_file(&model_path)?;
    let voices_bytes = ds_model::read_model_file(&voices_path)?;
    KokoroSynth::load(&model_bytes, &voices_bytes)
}

fn ensure_ort(model: ds_config::TtsModel) -> Result<(), String> {
    // Shared set only: absent CUDA-only assets must not block the load, which falls back to
    // the CPU profile (`ort_session::load_with_fallback`).
    if !ds_model::is_tts_model_present(model, false) {
        return Err(format!("{} model not downloaded", model.as_str()));
    }
    let pref = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into());
    let want_gpu = model.descriptor().wants_cuda(&pref);
    ds_model::ensure_ort_dylib_gpu(want_gpu).map(|_| ())
}

pub(crate) enum Backend {
    KokoroOrt(KokoroSynth),
    Chatterbox(Box<ChatterboxSynth>),
    Qwen(Box<QwenSynth>),
    OmniVoice(Box<OmniVoiceSynth>),
    #[cfg(target_os = "macos")]
    Mlx {
        model: ds_config::TtsModel,
        synth: ds_tts::synth_mlx::MlxTts,
    },
}

impl Backend {
    pub(crate) fn provider(&self) -> ds_config::RealizedProvider {
        match self {
            Self::KokoroOrt(synth) => synth.provider(),
            Self::Chatterbox(synth) => synth.provider(),
            Self::Qwen(synth) => synth.provider(),
            Self::OmniVoice(synth) => synth.provider(),
            #[cfg(target_os = "macos")]
            Self::Mlx { synth, .. } => synth.provider(),
        }
    }

    fn model(&self) -> ds_config::TtsModel {
        match self {
            Self::KokoroOrt(_) => ds_config::TtsModel::Kokoro,
            Self::Chatterbox(_) => ds_config::TtsModel::Chatterbox,
            Self::Qwen(_) => ds_config::TtsModel::Qwen,
            Self::OmniVoice(_) => ds_config::TtsModel::OmniVoice,
            #[cfg(target_os = "macos")]
            Self::Mlx { model, .. } => *model,
        }
    }

    fn synthesize_batch(
        &mut self,
        batch: &FrontendBatch,
        voice: &str,
        language: &str,
        rate: f32,
        params: &ds_config::ResolvedTtsParams,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        match (self, batch) {
            (Self::KokoroOrt(synth), FrontendBatch::Kokoro(batch)) => {
                synth.synthesize(batch.as_str(), voice, rate)
            }
            (Self::Chatterbox(synth), FrontendBatch::Text(chunk)) => {
                synth.synthesize(chunk, voice, language, params, cancelled)
            }
            (Self::Qwen(synth), FrontendBatch::Text(chunk)) => {
                synth.synthesize(chunk, voice, language, params, cancelled)
            }
            (Self::OmniVoice(synth), FrontendBatch::Text(chunk)) => {
                synth.synthesize(chunk, voice, language, params, cancelled)
            }
            #[cfg(target_os = "macos")]
            (Self::Mlx { synth, .. }, FrontendBatch::Kokoro(batch)) => {
                synth.synthesize(batch.as_str(), voice, language, rate, params)
            }
            #[cfg(target_os = "macos")]
            (Self::Mlx { synth, .. }, FrontendBatch::Text(chunk)) => {
                synth.synthesize(chunk, voice, language, rate, params)
            }
            _ => Err("frontend/backend model mismatch".to_string()),
        }
    }

    /// Discarded batch before READY (all providers); warm-up failure is best-effort.
    fn warm_up(&mut self) {
        let model = self.model();
        let (text, voice, language) = warmup_request(model);
        let batches = match frontend_batches_with_cancel(model, text, voice, language, &|| false) {
            Ok(FrontendOutcome::Finished(batches)) if batches.len() > 0 => batches,
            Ok(_) => return,
            Err(error) => {
                log::warn!(target: "helper", "{} frontend warm-up failed: {error}", model.as_str());
                return;
            }
        };
        let params = model.descriptor().resolve_params(&Default::default());
        if let Err(error) = prepare_backend_audio(
            self,
            &batches,
            &SynthesisRequest {
                skip: 0,
                voice,
                language,
                rate: 1.0,
                params: &params,
            },
            &|| false,
            |_| Ok(()),
        ) {
            log::warn!(target: "helper", "{} inference warm-up failed: {error}", model.as_str());
        }
    }
}

fn load_backend_unwarmed(model: ds_config::TtsModel) -> Result<Backend, String> {
    #[cfg(target_os = "macos")]
    if model
        .descriptor()
        .supports_provider(ds_config::Provider::Mlx)
    {
        let pref = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_default();
        if pref.eq_ignore_ascii_case("mlx") || pref.eq_ignore_ascii_case("auto") {
            match ds_tts::synth_mlx::MlxTts::load(model) {
                Ok(synth) => return Ok(Backend::Mlx { model, synth }),
                Err(error) => log::warn!(
                    target: "helper",
                    "MLX Audio TTS unavailable ({error}); falling back to ONNX"
                ),
            }
        }
    }
    match model {
        ds_config::TtsModel::Kokoro => Ok(Backend::KokoroOrt(load_kokoro()?)),
        ds_config::TtsModel::Chatterbox => {
            ensure_ort(model)?;
            Ok(Backend::Chatterbox(Box::new(ChatterboxSynth::load()?)))
        }
        ds_config::TtsModel::Qwen => {
            ensure_ort(model)?;
            Ok(Backend::Qwen(Box::new(QwenSynth::load()?)))
        }
        ds_config::TtsModel::OmniVoice => {
            ensure_ort(model)?;
            Ok(Backend::OmniVoice(Box::new(OmniVoiceSynth::load()?)))
        }
    }
}

pub(crate) fn load_backend() -> Result<Backend, String> {
    let mut backend = load_backend_unwarmed(tts_model())?;
    backend.warm_up();
    Ok(backend)
}

fn warmup_request(model: ds_config::TtsModel) -> (&'static str, &'static str, &'static str) {
    let descriptor = model.descriptor();
    (
        "Ready.",
        descriptor.warmup_voice,
        descriptor.default_language,
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FrontendBatch {
    Kokoro(g2p::KokoroPhonemeChunk),
    Text(String),
}

pub(crate) struct FrontendBatches {
    model: ds_config::TtsModel,
    batches: Vec<FrontendBatch>,
}

impl FrontendBatches {
    pub(crate) fn len(&self) -> usize {
        self.batches.len()
    }

    pub(crate) fn after_skip(&self, skip: usize) -> &[FrontendBatch] {
        &self.batches[skip.min(self.batches.len())..]
    }
}

pub(crate) enum FrontendOutcome {
    Finished(FrontendBatches),
    Cancelled,
}

/// One registry-driven frontend path for warm and one-shot synthesis. The representation
/// differs only at the model boundary (Kokoro consumes IPA; the added models consume text).
pub(crate) fn frontend_batches(
    model: ds_config::TtsModel,
    text: &str,
    voice: &str,
    language: &str,
) -> Result<FrontendOutcome, String> {
    frontend_batches_with_cancel(model, text, voice, language, &|| false)
}

pub(crate) fn frontend_batches_with_cancel(
    model: ds_config::TtsModel,
    text: &str,
    voice: &str,
    language: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<FrontendOutcome, String> {
    let batches = match model.descriptor().frontend {
        ds_config::TtsFrontend::KokoroPhonemes => {
            match g2p::phoneme_batches_for_cancellable(text, voice, language, cancelled)? {
                g2p::PhonemeBatchesOutcome::Finished(batches) => {
                    batches.into_iter().map(FrontendBatch::Kokoro).collect()
                }
                g2p::PhonemeBatchesOutcome::Cancelled => return Ok(FrontendOutcome::Cancelled),
            }
        }
        ds_config::TtsFrontend::PlainText => {
            if cancelled() {
                return Ok(FrontendOutcome::Cancelled);
            }
            let batches = ds_tts::chatterbox::frontend::text_chunks(text, language)
                .into_iter()
                .map(FrontendBatch::Text)
                .collect();
            if cancelled() {
                return Ok(FrontendOutcome::Cancelled);
            }
            batches
        }
    };
    Ok(FrontendOutcome::Finished(FrontendBatches {
        model,
        batches,
    }))
}

/// Shared prepare/commit fields for the first playable batch.
pub(crate) struct SynthesisRequest<'a> {
    pub(crate) skip: usize,
    pub(crate) voice: &'a str,
    pub(crate) language: &'a str,
    pub(crate) rate: f32,
    /// Complete validated params for the active model (`resolve_params` output).
    pub(crate) params: &'a ds_config::ResolvedTtsParams,
}

pub(crate) fn prepare_backend_audio(
    backend: &mut Backend,
    batches: &FrontendBatches,
    request: &SynthesisRequest<'_>,
    cancelled: &dyn Fn() -> bool,
    mut commit: impl FnMut(PreparedAudio) -> Result<(), String>,
) -> Result<PrepareOutcome, String> {
    if backend.model() != batches.model {
        return Err("frontend/backend model mismatch".to_string());
    }
    prepare_audio(
        batches.after_skip(request.skip),
        cancelled,
        |batch| {
            backend.synthesize_batch(
                batch,
                request.voice,
                request.language,
                request.rate,
                request.params,
                cancelled,
            )
        },
        &mut commit,
    )
}

pub(crate) fn run(text: &str, voice: &str, rate: f32) -> Result<(), String> {
    let model = tts_model();
    let language = ds_tts::detect_language(text, model);
    let FrontendOutcome::Finished(batches) = frontend_batches(model, text, voice, &language)?
    else {
        unreachable!("the one-shot frontend cannot cancel")
    };
    if batches.len() == 0 {
        return Ok(());
    }
    let mut player: Option<AudioPlayer> = None;
    let mut commit = |audio: crate::prepare::PreparedAudio| -> Result<(), String> {
        if player.is_none() {
            player = Some(AudioPlayer::open()?);
        }
        player.as_ref().expect("opened above").enqueue(audio.pcm);
        Ok(())
    };
    let mut backend = load_backend()?;
    // One-shot mode reads no config: descriptor defaults.
    let params = model.descriptor().resolve_params(&Default::default());
    prepare_backend_audio(
        &mut backend,
        &batches,
        &SynthesisRequest {
            skip: 0,
            voice,
            language: &language,
            rate,
            params: &params,
        },
        &|| false,
        &mut commit,
    )?;
    if let Some(player) = player {
        player.wait();
    }
    Ok(())
}

/// Dev diagnostic (`--synth-check`): drive the real load → detect → frontend → synth path
/// for one model + phrase and report amplitude, without opening the audio device. Reports
/// `non-finite`/`peak` so a NaN render (which reaches a sink as silence, not an error —
/// the Kokoro FP16 trap) fails loudly here instead of playing nothing. Loads UNwarmed so
/// the reported audio is this phrase, not the discarded warm-up utterance.
pub(crate) fn synth_check(
    model: ds_config::TtsModel,
    text: &str,
    voice: &str,
) -> Result<(), String> {
    let language = ds_tts::detect_language(text, model);
    let FrontendOutcome::Finished(batches) = frontend_batches(model, text, voice, &language)?
    else {
        return Err("frontend cancelled".to_string());
    };
    if batches.len() == 0 {
        return Err("frontend produced no batches".to_string());
    }
    let mut backend = load_backend_unwarmed(model)?;
    let mut pcm: Vec<f32> = Vec::new();
    // Descriptor defaults, deliberately ignoring config: the check reports the model's
    // baseline render (the same one the parity gates compare).
    let params = model.descriptor().resolve_params(&Default::default());
    prepare_backend_audio(
        &mut backend,
        &batches,
        &SynthesisRequest {
            skip: 0,
            voice,
            language: &language,
            rate: 1.0,
            params: &params,
        },
        &|| false,
        |audio| {
            pcm.extend_from_slice(&audio.pcm);
            Ok(())
        },
    )?;

    let health = audio_health(&pcm);
    println!(
        "OK model={} provider={} lang={language} voice={voice} samples={} peak={:.4} rms={:.4} ratio={:.3} non_finite={}",
        model.as_str(),
        backend.provider().as_str(),
        health.samples,
        health.peak,
        health.rms,
        health.crest,
        health.non_finite
    );
    health_verdict(&health)
}

/// Amplitude profile of one rendered utterance. `crest` is rms/peak (the INVERSE crest
/// factor): speech is peaky (low ratio), stationary noise/tones are not.
struct AudioHealth {
    samples: usize,
    non_finite: usize,
    peak: f32,
    rms: f32,
    crest: f32,
}

fn audio_health(pcm: &[f32]) -> AudioHealth {
    let samples = pcm.len();
    let mut non_finite = 0usize;
    let mut peak = 0.0f32;
    let mut sum_squares = 0.0f64;
    for &sample in pcm {
        if !sample.is_finite() {
            non_finite += 1;
            continue;
        }
        peak = peak.max(sample.abs());
        sum_squares += f64::from(sample) * f64::from(sample);
    }
    let finite = samples - non_finite;
    let rms = if finite > 0 {
        (sum_squares / finite as f64).sqrt() as f32
    } else {
        0.0
    };
    let crest = if peak > 0.0 { rms / peak } else { 0.0 };
    AudioHealth {
        samples,
        non_finite,
        peak,
        rms,
        crest,
    }
}

/// Ceilings measured on this workstation (release builds, one sentence each) before
/// pinning; speech sits well under both, degenerate renders well over:
///
/// | model     | provider | peak   | rms    | rms/peak |
/// |-----------|----------|--------|--------|----------|
/// | kokoro    | CPU      | 0.6775 | 0.0937 | 0.138    |
/// | kokoro    | CUDA     | 0.6702 | 0.0936 | 0.140    |
/// | omnivoice | CPU      | 0.8950 | 0.1190 | 0.133    |
/// | omnivoice | CUDA*    | 0.8950 | 0.1190 | 0.133    |
///
/// (*backbone on CUDA, Higgs decoder on CPU — #165.) Chatterbox/Qwen were not on this
/// disk; their bands are extrapolated from the same 24 kHz speech family and the
/// degenerate references (broken OmniVoice decode rendered rms 0.49 noise at ratio
/// ~0.58; a pure tone sits at 0.707). No ZCR gate: margins are wide (>3x) without it.
const RMS_CEILING: f32 = 0.35;
const RMS_PEAK_RATIO_CEILING: f32 = 0.45;

fn health_verdict(health: &AudioHealth) -> Result<(), String> {
    if health.samples == 0 {
        return Err("no samples".to_string());
    }
    if health.non_finite > 0 {
        return Err(format!(
            "{} non-finite samples — the render is silent",
            health.non_finite
        ));
    }
    if health.peak < 0.001 {
        return Err(format!("peak {:.6} is inaudible", health.peak));
    }
    if health.rms > RMS_CEILING {
        return Err(format!(
            "rms {:.3} exceeds {RMS_CEILING} — the render is noise, not speech",
            health.rms
        ));
    }
    if health.crest > RMS_PEAK_RATIO_CEILING {
        return Err(format!(
            "rms/peak {:.3} exceeds {RMS_PEAK_RATIO_CEILING} — stationary noise/tone, not speech",
            health.crest
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_token_is_case_insensitive_and_defaults_to_kokoro() {
        assert_eq!(
            model_from(Some("chatterbox")),
            ds_config::TtsModel::Chatterbox
        );
        assert_eq!(model_from(Some("qwen")), ds_config::TtsModel::Qwen);
        assert_eq!(
            model_from(Some("omnivoice")),
            ds_config::TtsModel::OmniVoice
        );
        // TtsModel::parse normalizes like every other config-token enum.
        assert_eq!(
            model_from(Some("Chatterbox")),
            ds_config::TtsModel::Chatterbox
        );
        assert_eq!(model_from(Some("bogus")), ds_config::TtsModel::Kokoro);
        assert_eq!(model_from(None), ds_config::TtsModel::Kokoro);
    }

    #[test]
    fn every_model_frontend_is_selected_by_the_registry() {
        for model in ds_config::TtsModel::ALL.iter().copied() {
            assert_eq!(
                model.descriptor().frontend == ds_config::TtsFrontend::KokoroPhonemes,
                model == ds_config::TtsModel::Kokoro,
                "{model:?}"
            );
        }
    }

    #[test]
    fn shared_batch_resume_slices_and_clamps_for_every_backend() {
        let batches = FrontendBatches {
            model: ds_config::TtsModel::Qwen,
            batches: ["a", "b", "c"]
                .into_iter()
                .map(|text| FrontendBatch::Text(text.to_string()))
                .collect(),
        };
        assert_eq!(batches.after_skip(0).len(), 3);
        assert_eq!(batches.after_skip(2).len(), 1);
        assert!(batches.after_skip(3).is_empty());
        assert!(batches.after_skip(usize::MAX).is_empty());
    }

    #[test]
    fn every_added_model_uses_the_same_ramped_text_frontend() {
        let text = "x".repeat(ds_tts::chatterbox::frontend::MAX_CHUNK_CHARS);
        for model in [
            ds_config::TtsModel::Chatterbox,
            ds_config::TtsModel::Qwen,
            ds_config::TtsModel::OmniVoice,
        ] {
            let outcome = frontend_batches_with_cancel(
                model,
                &text,
                model.descriptor().voices[0],
                model.descriptor().default_language,
                &|| false,
            )
            .unwrap();
            let FrontendOutcome::Finished(batches) = outcome else {
                panic!("a non-cancelled frontend must finish")
            };
            assert_eq!(batches.model, model);
            assert!(batches.len() > 1, "{model:?} must use ramped chunks");
            assert!(
                batches
                    .batches
                    .iter()
                    .all(|batch| matches!(batch, FrontendBatch::Text(_))),
                "{model:?} must use the registry-selected plain-text representation"
            );
            let FrontendBatch::Text(first) = &batches.batches[0] else {
                unreachable!()
            };
            assert_eq!(first.chars().count(), ds_tts::batch::STREAM_FIRST_BUDGET);
        }
    }

    /// Deterministic pseudo-noise in [-1, 1] (LCG; no rand dep, no seed drift).
    fn white_noise(len: usize) -> Vec<f32> {
        let mut state: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn white_noise_fails_the_amplitude_verdict() {
        // Uniform noise has rms/peak ~0.58 — over both ceilings.
        let health = audio_health(&white_noise(24_000));
        assert!(health.crest > 0.5, "measured {:.3}", health.crest);
        assert!(health_verdict(&health).is_err());
    }

    #[test]
    fn a_pure_tone_fails_the_ratio_gate_even_at_speech_loudness() {
        // sin has rms/peak = 0.707 regardless of amplitude; a quiet tone must still fail.
        let tone: Vec<f32> = (0..24_000)
            .map(|i| 0.2 * (i as f32 * 0.05).sin())
            .collect();
        let health = audio_health(&tone);
        assert!(health.rms < RMS_CEILING, "quiet tone passes the rms gate");
        let error = health_verdict(&health).unwrap_err();
        assert!(error.contains("rms/peak"), "{error}");
    }

    #[test]
    fn a_silence_padded_burst_passes_like_speech() {
        // Peaky content over mostly-silence — the amplitude shape speech has.
        let mut pcm = vec![0.0f32; 24_000];
        for (i, sample) in pcm[8_000..9_000].iter_mut().enumerate() {
            *sample = 0.7 * (i as f32 * 0.3).sin();
        }
        let health = audio_health(&pcm);
        assert_eq!(health_verdict(&health), Ok(()));
        assert!(health.crest < RMS_PEAK_RATIO_CEILING);
    }

    #[test]
    fn non_finite_and_empty_renders_fail() {
        let nan = audio_health(&vec![f32::NAN; 480]);
        assert_eq!(nan.non_finite, 480);
        assert!(health_verdict(&nan).unwrap_err().contains("non-finite"));

        let empty = audio_health(&[]);
        assert_eq!(empty.samples, 0);
        assert!(health_verdict(&empty).unwrap_err().contains("no samples"));

        // Finite but inaudible.
        let silent = audio_health(&vec![0.0002f32; 480]);
        assert!(health_verdict(&silent).unwrap_err().contains("inaudible"));
    }

    #[test]
    fn measured_speech_bands_pass_with_margin() {
        // The pinned table's worst row (omnivoice rms 0.119, ratio 0.133) modeled as a
        // synthetic profile: verdict must accept everything in the measured band.
        let health = AudioHealth {
            samples: 61_440,
            non_finite: 0,
            peak: 0.8950,
            rms: 0.1190,
            crest: 0.133,
        };
        assert_eq!(health_verdict(&health), Ok(()));
        // Degenerate reference: the broken decode's rms-0.49 noise fails both gates.
        let degenerate = AudioHealth {
            samples: 61_440,
            non_finite: 0,
            peak: 0.85,
            rms: 0.49,
            crest: 0.58,
        };
        assert!(health_verdict(&degenerate).is_err());
    }

    #[test]
    fn every_model_has_one_registry_driven_warmup_request() {
        for model in ds_config::TtsModel::ALL.iter().copied() {
            let (text, voice, language) = warmup_request(model);
            let descriptor = model.descriptor();
            assert_eq!(text, "Ready.");
            assert_eq!(voice, descriptor.warmup_voice);
            assert!(!voice.is_empty());
            assert_eq!(language, descriptor.default_language);
            assert!(descriptor.supports_language(language));
        }
    }
}
