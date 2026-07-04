//! Installer/setup hooks: the headless model/runtime prefetch driven by ds-model
//! (the single source of the pinned URLs/SHAs). The `--prefetch`/
//! `--install-prefetched`/`--print-manifest` arg dispatch lives in [`crate::main`].

/// Headless prefetch for the installer: fetch model assets and/or the Windows CUDA
/// runtime through ds-model (the single source of the pinned URLs/SHAs). Returns a
/// process exit code (0 ok, 1 failed). `what` = "models" | "cuda" | a per-model token;
/// anything unrecognized (including the no-arg "all" default) ⇒ both models + CUDA.
pub(crate) fn run_prefetch(what: &str) -> i32 {
    let p = |_done: u64, _total: u64| {};
    let models = || -> std::io::Result<()> {
        ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())?;
        ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
    };
    // The SAME platform gate as every other CUDA dispatcher (x86_64 Windows AND Linux —
    // see `DownloadTarget::is_supported_on_this_host`); this was windows-only, silently
    // no-opping a Linux `--prefetch cuda`. Off-platform stays a quiet Ok(()) skip (the
    // installer semantics), unlike the engine's per-target error.
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
        // `onnxruntime` — the base shared runtime dylib.
        Some(DownloadTarget::Onnxruntime) => {
            ds_model::ensure_onnxruntime_with_progress(&p).map(|_| ())
        }
        // `kokoro_model` — the full Kokoro model (+ ensures onnxruntime).
        Some(DownloadTarget::KokoroModel) => {
            ds_model::run_setup_kokoro_with_progress(&p).map(|_| ())
        }
        Some(DownloadTarget::ParakeetModel) => {
            ds_model::run_setup_parakeet_with_progress(&p).map(|_| ())
        } // parakeet (+ onnxruntime)
        // `sepformer_model` — the macOS speaker-lock separator (+ ensures onnxruntime).
        // Off-macOS this is a quiet Ok(()) skip, the installer semantics (see the CUDA
        // note above; the gate mirrors `DownloadTarget::is_supported_on_this_host`).
        Some(DownloadTarget::SepformerModel) => {
            if cfg!(target_os = "macos") {
                ds_model::run_setup_sepformer_with_progress(&p).map(|_| ())
            } else {
                Ok(())
            }
        }
        Some(DownloadTarget::Models) => models(),
        Some(DownloadTarget::Cuda) => cuda(),
        // Windows installer prerequisites (.NET / Windows App Runtime): the installer
        // downloads + runs these itself via the URLs from ds-model's manifest — ds-model
        // never installs them — so prefetch is a no-op here (guards against a stray
        // --install-prefetched falling through to the model fetch).
        Some(DownloadTarget::Dotnet) | Some(DownloadTarget::Winapp) => Ok(()),
        // Any other/unknown token — including the CLI's no-arg "all" default, which is no
        // longer a DownloadTarget (the engine sequences per-model fetches) — ⇒ both ONNX
        // models + the CUDA runtime, the historical installer default.
        _ => models().and_then(|_| cuda()),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            let msg = format!("ds-helper: prefetch '{what}' failed: {e}");
            eprintln!("{msg}");
            // stderr is discarded under the GUI subsystem (a GUI-subsystem caller can't read
            // it), so leave a diagnosable trace on disk the caller/user can find.
            let _ = std::fs::write(std::env::temp_dir().join("ds-prefetch-error.log"), &msg);
            1
        }
    }
}
