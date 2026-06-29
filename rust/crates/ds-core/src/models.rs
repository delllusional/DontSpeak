//! Model asset present-probes + a worker-thread download runner. UI-AGNOSTIC.
//!
//! Probes are cheap, network-free, and safe to call on the UI thread (they only
//! stat + sha-verify files under `model_dir()`). Downloads are the opposite:
//! they hit the network and take seconds-to-minutes, so [`download_with_callback`]
//! spawns a detached `std::thread` (the CORE owns the thread, not the caller)
//! and reports status via a `Fn(Progress)` callback. The callback fires on the
//! worker thread — a UI must marshal back to its UI thread itself.
//!
//! NOTHING here downloads during build or tests — `ensure`/`run_setup*` run only
//! from the worker, at runtime.

/// Which asset a download/delete row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    Kokoro,
    Onnxruntime,
    ParakeetOnnx,
}

/// A status update posted from the worker.
#[derive(Debug, Clone)]
pub struct Progress {
    pub asset: Asset,
    /// 0.0..=1.0 (indeterminate downloads report fractional best-effort or stay
    /// at 0.0 with a textual status — `ensure` does not expose byte progress).
    pub fraction: f32,
    pub status: String,
    pub done: bool,
    pub present: bool,
}

// ── Present-probes (cheap, network-free, UI-thread-safe) ─────────────────────

/// Is the full Kokoro asset set (onnx + voices + dylib) present + valid?
pub fn kokoro_present() -> bool {
    ds_model::kokoro_present()
}

/// Is the full Parakeet-ONNX asset set (encoder + decoder + preprocessor + vocab
/// + dylib) present + valid?
pub fn parakeet_onnx_present() -> bool {
    ds_model::parakeet_present()
}

/// Is the onnxruntime dylib present on disk?
pub fn onnxruntime_present() -> bool {
    ds_model::onnxruntime_dylib_path()
        .map(|p| p.is_file())
        .unwrap_or(false)
}

// ── Worker-thread download runner (the CORE owns the thread) ─────────────────

/// Spawn a detached worker that downloads `asset` and reports progress via
/// `on_progress`. Runs only at runtime, never during build/tests. The worker
/// calls the blocking `ds_model::run_setup*`; it posts a `done` message
/// (with the final presence) when it finishes or errors.
///
/// The callback runs on the WORKER thread (`Send + 'static` so it can cross the
/// thread boundary); the UI's closure must marshal back to its own thread.
///
/// The `_with_progress` ds-model entry points stream `(downloaded, total)` byte
/// counts (from each file's `Content-Length`, reported as one aggregate bar per
/// asset set), so we surface a real "X MB of Y MB" status + a determinate
/// fraction. The up-front manifest gives the total size before the first byte.
pub fn download_with_callback<F>(asset: Asset, on_progress: F)
where
    F: Fn(Progress) + Send + 'static,
{
    std::thread::spawn(move || {
        // Pre-download manifest → total size for the initial "0 of Y MB" label.
        let manifest = match asset {
            Asset::Kokoro => ds_model::kokoro_files(),
            Asset::Onnxruntime => ds_model::kokoro_files()
                .into_iter()
                .filter(|f| f.file_name.contains("onnxruntime"))
                .collect(),
            Asset::ParakeetOnnx => ds_model::parakeet_files(),
        };
        let manifest_total: u64 = manifest.iter().map(|f| f.size_bytes).sum();

        on_progress(Progress {
            asset,
            fraction: 0.0,
            status: format!("0 of {}", mb(manifest_total)),
            done: false,
            present: false,
        });

        // Worker-thread byte-progress sink, shared by all three asset kinds.
        let emit = |done: u64, total: u64| {
            let total = if total == 0 { manifest_total } else { total };
            let fraction = if total > 0 {
                (done as f32 / total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            on_progress(Progress {
                asset,
                fraction,
                status: format!("{} of {}", mb(done), mb(total)),
                done: false,
                present: false,
            });
        };

        let result: std::io::Result<()> = match asset {
            Asset::Kokoro => ds_model::run_setup_kokoro_with_progress(&emit).map(|_| ()),
            Asset::Onnxruntime => ds_model::ensure_onnxruntime_with_progress(&emit).map(|_| ()),
            Asset::ParakeetOnnx => ds_model::run_setup_parakeet_with_progress(&emit).map(|_| ()),
        };

        let (status, present) = match result {
            Ok(()) => ("Installed".to_string(), true),
            Err(e) => (format!("Failed: {e}"), false),
        };
        on_progress(Progress {
            asset,
            fraction: if present { 1.0 } else { 0.0 },
            status,
            done: true,
            present,
        });
    });
}

/// Format a byte count as a whole-MB label (MiB-based, matching the size labels).
fn mb(bytes: u64) -> String {
    format!("{} MB", bytes / 1_048_576)
}

/// Delete an asset's on-disk files (best-effort; missing files are a no-op).
/// Returns the new presence (false on success).
pub fn delete(asset: Asset) -> bool {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    match asset {
        Asset::Kokoro => {
            if let Some(p) = ds_model::model_path(ds_model::KOKORO_ONNX_FILE) {
                paths.push(p);
            }
            if let Some(p) = ds_model::model_path(ds_model::KOKORO_VOICES_FILE) {
                paths.push(p);
            }
        }
        Asset::Onnxruntime => {
            if let Some(p) = ds_model::onnxruntime_dylib_path() {
                paths.push(p);
            }
        }
        Asset::ParakeetOnnx => {
            for file in [
                ds_model::PARAKEET_ENCODER_FILE,
                ds_model::PARAKEET_DECODER_FILE,
                ds_model::PARAKEET_JOINER_FILE,
                ds_model::PARAKEET_TOKENS_FILE,
            ] {
                if let Some(p) = ds_model::model_path(file) {
                    paths.push(p);
                }
            }
        }
    }
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    // Re-probe presence after deletion.
    match asset {
        Asset::Kokoro => ds_model::kokoro_present(),
        Asset::Onnxruntime => onnxruntime_present(),
        Asset::ParakeetOnnx => ds_model::parakeet_present(),
    }
}
