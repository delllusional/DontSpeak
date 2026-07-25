//! Installer/setup hooks: headless model/runtime prefetch via ds-model (single
//! source of pinned URLs/SHAs). Arg dispatch for `--prefetch` /
//! `--install-prefetched` / `--print-manifest` lives in [`crate::main`].

/// Headless installer prefetch. Exit code 0/1. `what` = wire token or unknown
/// (including no-arg `"all"`) ⇒ models + CUDA.
pub(crate) fn run_prefetch(what: &str) -> i32 {
    log::info!(target: "helper", "ds-helper: prefetch '{what}' started");
    let p = |_done: u64, _total: u64| {};
    // One ambient resolution for every set fetch below (the installer boundary); an
    // unresolvable model dir is the same hard error the per-file path already reports.
    let hf_repos = |set: &[&'static ds_model::HfRepo]| -> std::io::Result<()> {
        let roots = ds_model::ModelRoots::ambient()
            .ok_or_else(|| std::io::Error::other("cannot resolve the model directory"))?;
        ds_model::hf_repo::ensure_hf_repos(&roots, set, &p)
    };
    let models = || -> std::io::Result<()> {
        ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())?;
        ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
    };
    // `#[cfg]` rather than the shared host gate below: `ensure_cuda_runtime_with_progress`
    // does not COMPILE off x86_64 Windows/Linux. The no-arg `"all"` arm reaches this without
    // a `DownloadTarget`, so the off-platform no-op closure has to exist.
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
        // ONE host gate for every named target, ahead of the fetch arms — the per-arm
        // `cfg!` copies this replaces were where the Apple-Silicon and macOS-any-arch
        // spellings drifted apart. Off-platform is a quiet `Ok(())` (installer semantics),
        // unlike the engine's per-target error.
        Some(target) if !target.is_supported_on_this_host() => Ok(()),
        Some(DownloadTarget::Onnxruntime) => {
            ds_model::ensure_onnxruntime_with_progress(&p).map(|_| ())
        }
        // Full portable Kokoro set (ONNX + voices + G2P + eSpeak loader + ORT).
        // Installers have no host CUDA probe; Kokoro has no cuda_files extras.
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
        // The shared voices npz (owned by KokoroModel) the ANE chain materializes packs
        // from, plus the two-root Core ML set.
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
        // Unknown (incl. no-arg "all") ⇒ models + CUDA, historical installer default.
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
            // GUI subsystem discards stderr; leave a diagnosable on-disk trace
            // when the unified log sink is not yet readable.
            let _ = std::fs::write(std::env::temp_dir().join("ds-prefetch-error.log"), &msg);
            1
        }
    }
}
