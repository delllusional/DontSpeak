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
        Some(DownloadTarget::KokoroModel) => {
            ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::KokoroFrontend) => {
            ds_model::run_setup_kokoro_frontend_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::ParakeetModel) => {
            ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
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
        // Legacy Windows prerequisite tokens: package is self-contained; aka.ms URLs were
        // unpinned and removed. No-ops for older installer invocations.
        Some(DownloadTarget::Dotnet) | Some(DownloadTarget::Winapp) => Ok(()),
        // Unknown (incl. no-arg "all") ⇒ models + CUDA, historical installer default.
        _ => models().and_then(|_| cuda()),
    };
    match r {
        Ok(()) => {
            log::info!(target: "helper", "ds-helper: prefetch '{what}' finished");
            0
        }
        Err(e) => {
            let msg = format!("ds-helper: prefetch '{what}' failed: {e}");
            log::warn!(target: "helper", "{}", msg);
            eprintln!("{msg}");
            // GUI subsystem discards stderr; leave a diagnosable on-disk trace.
            let _ = std::fs::write(std::env::temp_dir().join("ds-prefetch-error.log"), &msg);
            1
        }
    }
}
