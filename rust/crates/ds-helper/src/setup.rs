//! Installer/setup hooks: headless model/runtime prefetch via ds-model (single
//! source of pinned URLs/SHAs). Arg dispatch for `--prefetch` /
//! `--install-prefetched` / `--print-manifest` lives in [`crate::main`].

/// Headless installer prefetch. Exit code 0/1. `what` = wire token or unknown
/// (including no-arg `"all"`) ⇒ models + CUDA.
pub(crate) fn run_prefetch(what: &str) -> i32 {
    log::info!(target: "helper", "ds-helper: prefetch '{what}' started");
    let p = |_done: u64, _total: u64| {};
    let models = || -> std::io::Result<()> {
        ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())?;
        ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
    };
    // Same platform gate as every CUDA dispatcher (`DownloadTarget::is_supported_on_this_host`);
    // was windows-only and silently no-op'd Linux `--prefetch cuda`. Off-platform: quiet Ok(())
    // (installer semantics), unlike the engine's per-target error.
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
        ) => {
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                ds_model::mlx_repo::ensure_mlx_repos(
                    ds_model::mlx_repo::tts_mlx_set(
                        target.tts_model().expect("MLX TTS target has a model"),
                    ),
                    &p,
                )
            } else {
                Ok(())
            }
        }
        Some(DownloadTarget::ParakeetMlx) => {
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                ds_model::mlx_repo::ensure_mlx_repos(&ds_model::mlx_repo::PARAKEET_MLX_SET, &p)
            } else {
                Ok(())
            }
        }
        Some(DownloadTarget::DiarizationMlx) => {
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                ds_model::mlx_repo::ensure_mlx_repos(&ds_model::mlx_repo::DIARIZATION_MLX_SET, &p)
            } else {
                Ok(())
            }
        }
        // Off-macOS quiet skip (installer semantics; mirrors `is_supported_on_this_host`).
        Some(DownloadTarget::SepformerModel) => {
            if cfg!(target_os = "macos") {
                ds_model::run_setup_sepformer_with_progress(&p).map(|_| ())
            } else {
                Ok(())
            }
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
