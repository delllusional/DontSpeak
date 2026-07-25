//! Headless model/runtime prefetch via ds-model (pinned URLs/SHAs).
//! Arg dispatch for `--prefetch` / `--install-prefetched` / `--print-manifest` is in
//! [`crate::main`].

/// Installer prefetch. Exit 0/1. Wire token, or unknown/`"all"` → models + CUDA.
pub(crate) fn run_prefetch(what: &str) -> i32 {
    log::info!(target: "helper", "ds-helper: prefetch '{what}' started");
    let p = |_done: u64, _total: u64| {};
    // One ambient ModelRoots for every set fetch (installer boundary).
    let hf_repos = |set: &[&'static ds_model::HfRepo]| -> std::io::Result<()> {
        let roots = ds_model::ModelRoots::ambient()
            .ok_or_else(|| std::io::Error::other("cannot resolve the model directory"))?;
        ds_model::hf_repo::ensure_hf_repos(&roots, set, &p)
    };
    let models = || -> std::io::Result<()> {
        ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())?;
        ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
    };
    // `#[cfg]`: `ensure_cuda_runtime_with_progress` only compiles on x86_64 Win/Linux.
    // `"all"` reaches here without a DownloadTarget, so off-platform needs a no-op.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    let cuda =
        || -> std::io::Result<()> { ds_model::ensure_cuda_runtime_with_progress(&p).map(|_| ()) };
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    let cuda = || -> std::io::Result<()> { Ok(()) };
    use ds_model::DownloadTarget;
    let r = match DownloadTarget::parse(what) {
        // Shared host gate before fetch arms (replaces per-arm cfg! drift).
        // Off-platform → quiet Ok (installer semantics, not engine errors).
        Some(target) if !target.is_supported_on_this_host() => Ok(()),
        Some(DownloadTarget::Onnxruntime) => {
            ds_model::ensure_onnxruntime_with_progress(&p).map(|_| ())
        }
        // Portable Kokoro set (ONNX + voices + G2P + eSpeak loader + ORT).
        Some(DownloadTarget::KokoroModel) => {
            ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::KokoroFrontend) => {
            ds_model::run_setup_kokoro_frontend_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::ParakeetModel) => {
            ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::ChatterboxModel) => {
            ds_model::run_setup_tts_model_with_progress(ds_config::TtsModel::Chatterbox, false, &p)
                .map(|_| ())
        }
        Some(DownloadTarget::QwenModel) => {
            ds_model::run_setup_tts_model_with_progress(ds_config::TtsModel::Qwen, false, &p)
                .map(|_| ())
        }
        Some(DownloadTarget::OmniVoiceModel) => {
            ds_model::run_setup_tts_model_with_progress(ds_config::TtsModel::OmniVoice, false, &p)
                .map(|_| ())
        }
        Some(
            target @ (DownloadTarget::KokoroMlx
            | DownloadTarget::ChatterboxMlx
            | DownloadTarget::QwenMlx
            | DownloadTarget::OmniVoiceMlx),
        ) => hf_repos(ds_model::mlx_repo::tts_mlx_set(
            target.tts_model().expect("MLX TTS target has a model"),
        )),
        Some(DownloadTarget::ParakeetMlx) => hf_repos(&ds_model::mlx_repo::PARAKEET_MLX_SET),
        Some(DownloadTarget::DiarizationMlx) => hf_repos(&ds_model::mlx_repo::DIARIZATION_MLX_SET),
        // Shared voices npz + Core ML two-root set (ANE materialize source).
        Some(DownloadTarget::KokoroFluid) => {
            ds_model::ensure_with_progress(&ds_model::kokoro_voices_spec(), &p)
                .and_then(|_| hf_repos(&ds_model::coreml_repo::KOKORO_COREML_SET))
        }
        Some(DownloadTarget::ParakeetFluid) => {
            hf_repos(&ds_model::coreml_repo::PARAKEET_COREML_SET)
        }
        Some(DownloadTarget::DiarizationFluid) => {
            hf_repos(&ds_model::coreml_repo::DIARIZATION_COREML_SET)
        }
        Some(DownloadTarget::SepformerModel) => {
            ds_model::run_setup_sepformer_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::Models) => models(),
        Some(DownloadTarget::Cuda) => cuda(),
        // Unknown / no-arg "all" → models + CUDA (installer default).
        None => models().and_then(|_| cuda()),
    };
    match r {
        Ok(()) => {
            log::info!(target: "helper", "ds-helper: prefetch '{what}' finished");
            0
        }
        Err(e) => {
            let msg = format!("ds-helper: prefetch '{what}' failed: {e}");
            log::warn!(target: "helper", "{msg}");
            // GUI may discard stderr; leave an on-disk trace.
            let _ = std::fs::write(std::env::temp_dir().join("ds-prefetch-error.log"), &msg);
            1
        }
    }
}
