//! onnxruntime runtime bootstrap: resolve the load-dynamic dylib, version-gate
//! it, and fetch the version-matched prebuilt (route A) — plus the optional
//! Windows CUDA GPU runtime. The SINGLE place the `ORT_DYLIB_PATH` env is set.

use std::path::PathBuf;

use crate::archive::extract_runtime_member;
use crate::download::{DEFAULT_RETRIES, download_to, is_permanent_error};
use crate::hash::verify_sha256;
use crate::model_path;

/// The libonnxruntime dylib file name `ort` (load-dynamic) defaults to on this
/// OS, so a bare `ORT_DYLIB_PATH` lands on the right name. We download into
/// `model_dir()/<this>` and point `ORT_DYLIB_PATH` at the absolute path.
pub fn onnxruntime_dylib_file() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "libonnxruntime.so"
    }
}

/// Source for the prebuilt onnxruntime `.tgz`/`.zip` (route A): URL + pinned
/// SHA-256 of the archive. ONNX Runtime 1.27.0 (see `onnxruntime_dist` for the
/// api-24 compatibility rationale).
pub(crate) struct OrtDist {
    pub(crate) url: &'static str,
    /// SHA-256 of the downloaded archive (`.tgz` on macOS, `.zip` on Windows).
    pub(crate) archive_sha256: &'static str,
}

/// The onnxruntime archive distribution for THIS target, or `None` on an
/// unsupported platform (the caller then documents route B / a manual dylib).
/// All pins are ONNX Runtime **1.27.0**. The workspace `ort` pin is still api-24
/// (ORT_API_VERSION 24, what ort-sys 2.0.0-rc.12 / transcribe-rs compile against), and a
/// NEWER runtime serves an older API request — `GetApi(24)` succeeds on a 1.27 dylib
/// (verified on-device). We moved OFF 1.24.2 because its model loader DEADLOCKS while
/// loading the SepFormer speaker-separation graph (the dictation speaker-lock); 1.27 loads
/// it in <1 s. Kokoro/Parakeet are unaffected (backward-compatible; on Apple Silicon they
/// run on Core ML / ANE, not this dylib, anyway).
pub(crate) fn onnxruntime_dist() -> Option<OrtDist> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        // Microsoft's official release ships the DYNAMIC libonnxruntime.dylib.
        // (The pyke ortrs archive ships only a STATIC libonnxruntime.a, which
        // ort's `load-dynamic` cannot dlopen.)
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        // Microsoft's official win-x64 build — a .zip whose lib/onnxruntime.dll is
        // the dynamic runtime `ort` (load-dynamic) dlopens via ORT_DYLIB_PATH.
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        // Microsoft's official win-arm64 build — same .zip layout (lib/onnxruntime.dll) as
        // win-x64, dlopened by `ort` (load-dynamic). Native ARM64, no x64 emulation.
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // Microsoft's official linux-x64 build — a .tgz whose lib/libonnxruntime.so.1.27.0 is
        // the dynamic runtime `ort` (load-dynamic) dlopens via ORT_DYLIB_PATH.
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    // Intel macOS / other Linux arches: no pinned dynamic dist → route B (build ort with
    // `download-binaries`, or set ORT_DYLIB_PATH manually).
    #[allow(unreachable_code)]
    None
}

/// The resolved path the onnxruntime dylib lives at, for the caller to set `ORT_DYLIB_PATH`.
///
/// Prefers an externally supplied `ORT_DYLIB_PATH` when it points at an existing file — a notarized
/// macOS build bundles the dylib in the app and sets this to `Contents/Frameworks/libonnxruntime.dylib`
/// (signed + notarized with the app, so there's no runtime download to be Gatekeeper-quarantined).
/// When the env var is unset/missing it falls back to the downloaded copy under `model_dir()`, so the
/// default (local) behaviour is unchanged. `None` only if neither resolves.
pub fn onnxruntime_dylib_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    model_path(onnxruntime_dylib_file())
}

/// The onnxruntime version the workspace `ort` pin (api-24) requires at runtime —
/// it's embedded in the dylib's `LC_ID_DYLIB` name (`libonnxruntime.<VER>.dylib`).
/// Defined in the download registry (`urls.rs`); re-exported here for the historical path.
pub use crate::urls::ONNXRUNTIME_VERSION;

/// CHEAP check that the on-disk onnxruntime dylib is the version `ort` needs.
///
/// A WRONG-version dylib (e.g. a stale 1.22) `dlopen`s fine, but `GetApi(24)` then
/// returns NULL and `ort` rc.12 RE-ENTERS its `api()` OnceLock while building the
/// error → a self-deadlock (the engine's warm child hangs before READY, in a
/// respawn loop). So we reject a mismatched dylib BEFORE handing it to `ort`. We
/// read only the Mach-O header region (load commands, where `LC_ID_DYLIB` lives —
/// the first few KB), cheap enough for the status-poll path.
///
/// NAMING NOTE: onnxruntime ≥ 1.25 ships a MAJOR-ONLY `LC_ID_DYLIB`
/// (`libonnxruntime.1.dylib`) — the full `1.27.0` string lives deep in the binary, not the
/// cheap-to-read header. Older 1.24.x used the FULL `libonnxruntime.1.24.2.dylib`. So we
/// match the major-only id: it accepts our pinned new-style 1.27 dylib and REJECTS an
/// old-style full-version dylib (e.g. a stale 1.24.2), which then triggers a re-download.
/// A precise version is enforced upstream by the SHA-256-pinned archive `ensure` downloads.
pub fn onnxruntime_dylib_version_ok() -> bool {
    let Some(path) = onnxruntime_dylib_path() else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        use std::io::Read;
        let Ok(mut f) = std::fs::File::open(&path) else {
            return false;
        };
        let mut buf = [0u8; 65536];
        let n = f.read(&mut buf).unwrap_or(0);
        // Major-only id (new convention). Bounded by `.dylib` so it can't also match the
        // prefix of a full-version id like `libonnxruntime.1.24.2.dylib`.
        let needle = b"libonnxruntime.1.dylib";
        buf[..n].windows(needle.len()).any(|w| w == needle)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows/Linux: the dll/.so is extracted from the SHA-pinned
        // ONNXRUNTIME_VERSION package into this managed path, so its PRESENCE
        // implies the right version — there is no embedded dylib-id string to scan,
        // and re-reading a 16 MB Windows dll on every status poll would be wasteful.
        path.is_file()
    }
}

/// Resolve the onnxruntime dylib, verify it's the version `ort` needs, and point
/// `ort` (load-dynamic) at it via `ORT_DYLIB_PATH`. The SINGLE bootstrap shared by
/// every in-process ONNX backend — Kokoro-ONNX (TTS) and Parakeet-ONNX (STT) — so
/// the resolve + version-gate + the exact error string live in ONE place. Returns
/// the dylib path on success, or a user-facing error (no dylib / wrong version).
///
/// Rejecting a wrong-version dylib BEFORE `ort` touches it is load-bearing: a
/// mismatched `GetApi` makes ort rc.12 self-deadlock (it re-enters its `api`
/// OnceLock) instead of erroring. The Windows-CUDA path resolves a different
/// (GPU) dylib itself and sets `ORT_DYLIB_PATH` after; it doesn't use this.
pub fn ensure_ort_dylib() -> Result<PathBuf, String> {
    let path = onnxruntime_dylib_path().ok_or("cannot resolve onnxruntime dylib path")?;
    if !onnxruntime_dylib_version_ok() {
        return Err(format!(
            "onnxruntime dylib is not {ONNXRUNTIME_VERSION} — re-download it in Settings › Models"
        ));
    }
    set_ort_dylib_path(&path);
    Ok(path)
}

/// Resolve the onnxruntime dylib for an in-process ONNX engine and point `ort` at it,
/// choosing the Windows CUDA **GPU** runtime when `want_gpu` AND that runtime is present,
/// else the CPU (version-gated) dylib. On the GPU path it also prepends the CUDA DLL dir
/// to `PATH` exactly once (the Windows loader resolves the CUDA/cuDNN DLLs from there).
/// Returns the chosen dylib path.
///
/// The SINGLE GPU-aware ORT bootstrap shared by BOTH ONNX engines — Kokoro (TTS, via the
/// warm helper's `load_synth`) and Parakeet (STT, via `ParakeetTranscriber`). They run in
/// ONE warm-helper process over ONE ort runtime, so routing both through here keeps their
/// CUDA path identical: whichever loads first `dlopen`s the GPU onnxruntime and the other
/// reuses it. Falls back to [`ensure_ort_dylib`] (CPU + version gate) whenever GPU isn't
/// wanted/available, so it never breaks dictation or playback.
pub fn ensure_ort_dylib_gpu(want_gpu: bool) -> Result<PathBuf, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    if want_gpu
        && cuda_driver_present()
        && cuda_runtime_present()
        && let Some(gpu_dll) = cuda_onnxruntime_path()
    {
        if let Some(dir) = cuda_runtime_dir() {
            // Prepend the CUDA dir to PATH EXACTLY ONCE, before ort dlopens the GPU
            // onnxruntime (Windows LoadLibrary reads PATH live to find the CUDA/cuDNN
            // DLLs). The Once is what makes this safe even though TTS/STT now warm up on
            // parallel threads: the write happens at most once and is the only PATH writer.
            static PATH_ONCE: std::sync::Once = std::sync::Once::new();
            PATH_ONCE.call_once(|| {
                let old = std::env::var("PATH").unwrap_or_default();
                // SAFETY: the Once serializes this to a single execution and there is no other
                // concurrent PATH writer in-process.
                unsafe { std::env::set_var("PATH", format!("{};{old}", dir.display())) };
            });
        }
        set_ort_dylib_path(&gpu_dll);
        return Ok(gpu_dll);
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if want_gpu
        && cuda_driver_present()
        && cuda_runtime_present()
        && let Some(gpu_so) = cuda_onnxruntime_path()
    {
        // Make the CUDA dependency libs resolvable for the provider plugin ORT will dlopen.
        if let Some(dir) = cuda_runtime_dir() {
            preload_cuda_libs(&dir);
        }
        set_ort_dylib_path(&gpu_so);
        return Ok(gpu_so);
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    let _ = want_gpu;
    ensure_ort_dylib()
}

/// Linux: dlopen every CUDA *dependency* `.so` in the flat runtime dir with `RTLD_GLOBAL`, so
/// the CUDA execution-provider plugin resolves libcudart/cublas/cudnn/... from the GLOBAL
/// symbol namespace (glibc reads `LD_LIBRARY_PATH` only at process start, so we can't add the
/// dir after launch). Multi-pass: retry the ones that fail (unresolved deps) until no further
/// progress — loads them in dependency order without hardcoding it. ORT loads its own
/// `libonnxruntime*.so` (via `ORT_DYLIB_PATH`) and the sibling provider plugins, so those are
/// skipped here. Best-effort + runs ONCE; the GPU session build still falls back to CPU if the
/// provider can't initialize. (Validate on real NVIDIA hardware — untestable without a GPU.)
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn preload_cuda_libs(dir: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    static PRELOAD_ONCE: std::sync::Once = std::sync::Once::new();
    PRELOAD_ONCE.call_once(|| {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut pending: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains(".so") && !n.starts_with("libonnxruntime"))
                    .unwrap_or(false)
            })
            .collect();
        loop {
            let before = pending.len();
            pending.retain(|p| {
                let Ok(c) = CString::new(p.as_os_str().as_bytes()) else {
                    return false;
                };
                // RTLD_NOW so a success means every dep resolved; RTLD_GLOBAL so the symbols are
                // visible to the provider plugin ORT loads later.
                let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
                h.is_null() // keep (retry next pass) if it failed
            });
            if pending.is_empty() || pending.len() == before {
                break; // all loaded, or no further progress
            }
        }
    });
}

/// Make an ORT [`SessionBuilder`](ort::session::builder::SessionBuilder) with the CUDA execution
/// provider registered, and the [`RealizedProvider`](ds_config::RealizedProvider) it loaded on. THE
/// single GPU-aware session-builder shared by Kokoro TTS (`ds_tts::synth`) and Parakeet STT
/// (`ds_stt::streaming`), so the CUDA-EP registration + CPU fallback lives in ONE place instead of
/// being copy-pasted per engine — the two can't drift into different GPU behavior.
///
/// It only ATTEMPTS CUDA when `want_gpu` AND the GPU runtime + NVIDIA driver are actually present
/// (the SAME gate [`ensure_ort_dylib_gpu`] uses to pick the GPU dylib). This is load-bearing for
/// HONESTY: `resolved_*_provider` returns `cuda` on every x64 box as a static preference, so without
/// this gate a CPU-only user (no ~1.5 GB GPU runtime) would report `Cuda` while the session actually
/// ran on CPU — the "UI claims CUDA but runs CPU" trap. Gated, a returned `Cuda` means the runtime +
/// driver are installed AND the EP registered. When the runtime IS present but the EP still fails
/// (driver/runtime mismatch, provider-DLL init — Win32 1114) the REAL ort error is logged before the
/// CPU fallback, so that genuine failure stays diagnosable. `want_gpu` is ignored off Windows/Linux-
/// x64 (no CUDA EP there); the caller's macOS Core ML path is separate.
pub fn cuda_session_builder(
    want_gpu: bool,
) -> Result<(ort::session::builder::SessionBuilder, ds_config::RealizedProvider), String> {
    use ds_config::RealizedProvider;
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
    if want_gpu && cuda_runtime_present() && cuda_driver_present() {
        use ort::execution_providers::CUDAExecutionProvider;
        // ort's builder methods return the builder INSIDE their error (for recovery), so chain
        // them with `?` in a closure that yields ort::Result and match on the whole GPU attempt.
        match (|| -> ort::Result<_> {
            let b = ort::session::Session::builder()?;
            // `.error_on_failure()`: WITHOUT it, ort registers the CUDA EP best-effort and returns
            // Ok even when registration SOFT-fails (device/provider unavailable), so the session
            // would silently commit on CPU while we returned `Cuda` — a mislabel. With it, a
            // registration failure propagates as `Err`, so the CPU fallback below fires AND the token
            // returned is honestly `Cpu`.
            Ok(b.with_execution_providers([CUDAExecutionProvider::default().build().error_on_failure()])?)
        })() {
            Ok(b) => return Ok((b, RealizedProvider::Cuda)),
            Err(e) => {
                eprintln!("dontspeak/helper: CUDA EP registration failed — running on CPU: {e}")
            }
        }
    }
    let _ = want_gpu;
    let b = ort::session::Session::builder().map_err(|e| format!("ort session builder: {e}"))?;
    Ok((b, RealizedProvider::Cpu))
}

/// Point `ort` (load-dynamic) at `path` by writing `ORT_DYLIB_PATH`. The SINGLE
/// place this env var is set: [`ensure_ort_dylib`] routes the CPU/version-gated
/// dylib through here, and the Windows-CUDA path (which resolves its own GPU dylib,
/// bypassing the version gate) calls this directly — so the one `unsafe set_var`
/// and its threading argument live in ONE spot instead of being duplicated per
/// in-process ONNX backend. Call BEFORE the first ort session is built; idempotent.
pub fn set_ort_dylib_path(path: &std::path::Path) {
    // SERIALIZED behind a Once: TTS (Kokoro) and STT (Parakeet) now warm up on PARALLEL
    // threads, and BOTH route their ort bootstrap through here — so the env write must happen
    // at most once and never concurrently (`set_var` is not thread-safe; a concurrent write +
    // ort's lazy read of ORT_DYLIB_PATH would be a data race / UB). The dylib path is
    // deterministic per process (both engines load the SAME runtime), so first-wins is correct.
    static DYLIB_ONCE: std::sync::Once = std::sync::Once::new();
    DYLIB_ONCE.call_once(|| {
        // SAFETY: the Once guarantees this runs exactly once and never races another writer
        // of ORT_DYLIB_PATH; ort reads the var lazily when it builds its first session.
        unsafe { std::env::set_var("ORT_DYLIB_PATH", path) };
    });
}

/// Ensure the onnxruntime dylib exists locally under `model_dir()` (route A).
/// If already present, returns its path. Otherwise downloads the version-matched
/// `.tgz` to a temp file (verifying its pinned SHA-256), extracts the single
/// `libonnxruntime*.dylib` (/.so/.dll) member, and atomically renames it onto
/// the final dylib path. Returns an `Unsupported` error on a platform with no
/// pinned distribution (the README documents route B there).
pub fn ensure_onnxruntime() -> std::io::Result<PathBuf> {
    ensure_onnxruntime_with_progress(&|_, _| {})
}

/// Like [`ensure_onnxruntime`] but reports the `.tgz` download progress.
pub fn ensure_onnxruntime_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let final_path = onnxruntime_dylib_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    if final_path.is_file() {
        return Ok(final_path);
    }
    let Some(dist) = onnxruntime_dist() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no pinned onnxruntime distribution for this platform; \
             build ort with --features download-binaries (route B) or set \
             ORT_DYLIB_PATH to a manually installed libonnxruntime",
        ));
    };
    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("dylib path has no parent"))?;
    std::fs::create_dir_all(dir)?;

    // Download the .tgz, verify ITS sha (the archive digest), extract the single
    // dylib member — all under the SAME retry policy as the model files: transient
    // failures (truncation/timeout/5xx) retry with backoff; permanent ones
    // (complete-body sha mismatch / 404) fail fast.
    let retries = DEFAULT_RETRIES.max(1);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries {
        let tmp_tgz = tempfile::NamedTempFile::new_in(dir)?;
        let result = (|| -> std::io::Result<()> {
            download_to(dist.url, tmp_tgz.path(), progress)?;
            if !verify_sha256(tmp_tgz.path(), dist.archive_sha256) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "onnxruntime archive sha256 mismatch",
                ));
            }
            extract_runtime_member(tmp_tgz.path(), &final_path)
        })();
        match result {
            Ok(()) => return Ok(final_path),
            Err(e) => {
                if is_permanent_error(&e) {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("permanent onnxruntime download failure (not retried): {e}"),
                    ));
                }
                last_err = Some(std::io::Error::new(
                    e.kind(),
                    format!("onnxruntime attempt {} of {retries}: {e}", attempt + 1),
                ));
                if attempt + 1 < retries {
                    std::thread::sleep(std::time::Duration::from_millis(
                        500 * (attempt as u64 + 1),
                    ));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("onnxruntime download failed")))
}

// ── CUDA GPU runtime (download-on-demand) — Windows + Linux x86_64 ────────────
//
// The warm Kokoro child (and Parakeet STT) can run on an NVIDIA GPU (2.8-4.6x faster than
// CPU, validated on Pascal). The CUDA execution provider needs a separate, larger runtime
// than the CPU dylib: the GPU onnxruntime + CUDA 12.6 + cuDNN 9.5 libs. We fetch them ON
// DEMAND (only when GPU is selected) from the pinned PyPI wheels (`urls::CUDA_WHEELS`) into
// `model_dir()/cuda/`, then point ORT_DYLIB_PATH at the GPU runtime. Windows then prepends the
// dir to PATH; Linux preloads the dependency .so's RTLD_GLOBAL (see `ensure_ort_dylib_gpu`).
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub(crate) use crate::urls::CUDA_WHEELS;

/// The dir (under `model_dir()`) holding the GPU CUDA runtime libs — kept separate from the
/// CPU runtime so the two never clash.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub fn cuda_runtime_dir() -> Option<PathBuf> {
    model_path("cuda")
}

/// The GPU onnxruntime path (set `ORT_DYLIB_PATH` to this for CUDA). Windows: a fixed
/// `onnxruntime.dll`. Linux: the versioned `libonnxruntime.so.<ver>` the wheel ships, found by
/// scanning the runtime dir for the core lib (excluding the `_providers_*` plugins).
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub fn cuda_onnxruntime_path() -> Option<PathBuf> {
    let dir = cuda_runtime_dir()?;
    #[cfg(target_os = "windows")]
    {
        Some(dir.join("onnxruntime.dll"))
    }
    #[cfg(target_os = "linux")]
    {
        cuda_core_runtime_so(&dir)
    }
}

/// Linux: find the CORE GPU runtime `.so` in `dir` — a `libonnxruntime.so*` that is NOT a
/// `libonnxruntime_providers_*` plugin (note the '.' after libonnxruntime vs the '_').
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn cuda_core_runtime_so(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.starts_with("libonnxruntime.")
                        && n.contains(".so")
                        && !n.contains("_providers")
                })
                .unwrap_or(false)
        })
}

/// Is the CUDA GPU runtime already fetched (cheap presence check)?
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub fn cuda_runtime_present() -> bool {
    let Some(dir) = cuda_runtime_dir() else {
        return false;
    };
    #[cfg(target_os = "windows")]
    {
        dir.join("onnxruntime.dll").is_file()
            && dir.join("onnxruntime_providers_cuda.dll").is_file()
            && dir.join("cudnn64_9.dll").is_file()
    }
    #[cfg(target_os = "linux")]
    {
        // The core runtime + the CUDA provider plugin + a cuDNN 9 lib all extracted.
        cuda_core_runtime_so(&dir).is_some()
            && dir.join("libonnxruntime_providers_cuda.so").is_file()
            && std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok()).any(|e| {
                        e.file_name()
                            .to_str()
                            .map(|n| n.starts_with("libcudnn.so"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
    }
}

/// Whether an NVIDIA GPU **driver** is installed — the cheap, side-effect-free pre-check that
/// gates the ~1.4 GB CUDA-runtime download AND the GPU-dylib selection. The DRIVER ships
/// `libcuda.so.1` (Linux) / `nvcuda.dll` (Windows), which are NOT part of the downloadable
/// onnxruntime-gpu wheels (those carry cudart/cublas/cudnn — the driver comes WITH the card). So
/// a present driver lib ⇒ a real NVIDIA GPU + driver are installed and CUDA is worth pursuing; if
/// it's absent, CUDA is unsupported on this box, so we neither pull the big runtime nor try to
/// load the GPU execution provider — it would only fail and fall back to CPU anyway.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub fn cuda_driver_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        // dlopen the driver lib through the loader's normal search path; a non-null handle ⇒ the
        // NVIDIA driver is installed. Close it again — this is only a presence probe.
        let name = c"libcuda.so.1";
        let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if h.is_null() {
            false
        } else {
            unsafe { libc::dlclose(h) };
            true
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::FreeLibrary;
        use windows::Win32::System::LibraryLoader::LoadLibraryW;
        use windows::core::w;
        // LoadLibraryW("nvcuda.dll") through the OS's standard DLL search order (NO hardcoded
        // path) — a successful load ⇒ the NVIDIA driver is installed. Free it again: this is a
        // live presence probe, evaluated each time, never cached at a stale moment.
        match unsafe { LoadLibraryW(w!("nvcuda.dll")) } {
            Ok(h) => {
                let _ = unsafe { FreeLibrary(h) };
                true
            }
            Err(_) => false,
        }
    }
}

/// Download + extract the pinned CUDA runtime wheels into [`cuda_runtime_dir`].
/// Each wheel is a zip; we pull out every `*.dll`. Idempotent (a present runtime
/// returns immediately). `progress(done_wheels, total_wheels)`. ~1.4 GB on first
/// fetch — the caller (GPU opt-in) gates this.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub fn ensure_cuda_runtime_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    // Windows wheels carry .dll's, Linux wheels carry .so's — same flatten-into-one-dir flow.
    #[cfg(target_os = "windows")]
    use crate::archive::extract_all_dlls as extract_libs;
    #[cfg(target_os = "linux")]
    use crate::archive::extract_all_sos as extract_libs;
    let dir = cuda_runtime_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    if cuda_runtime_present() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir)?;
    let total = CUDA_WHEELS.len() as u64;
    for (idx, (url, sha)) in CUDA_WHEELS.iter().enumerate() {
        progress(idx as u64, total);
        let retries = DEFAULT_RETRIES.max(1);
        let mut last_err: Option<std::io::Error> = None;
        let mut done = false;
        for attempt in 0..retries {
            let tmp = tempfile::NamedTempFile::new_in(&dir)?;
            let r = (|| -> std::io::Result<()> {
                download_to(url, tmp.path(), &|_, _| {})?;
                if !verify_sha256(tmp.path(), sha) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "cuda wheel sha256 mismatch",
                    ));
                }
                extract_libs(tmp.path(), &dir)
            })();
            match r {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    if is_permanent_error(&e) {
                        return Err(std::io::Error::new(
                            e.kind(),
                            format!("permanent cuda runtime download failure: {e}"),
                        ));
                    }
                    last_err = Some(e);
                    if attempt + 1 < retries {
                        std::thread::sleep(std::time::Duration::from_millis(
                            500 * (attempt as u64 + 1),
                        ));
                    }
                }
            }
        }
        if !done {
            return Err(
                last_err.unwrap_or_else(|| std::io::Error::other("cuda runtime download failed"))
            );
        }
    }
    progress(total, total);
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onnxruntime_dylib_file_name_is_per_os() {
        let name = onnxruntime_dylib_file();
        #[cfg(target_os = "macos")]
        assert_eq!(name, "libonnxruntime.dylib");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(name, "libonnxruntime.so");
        assert!(!name.is_empty());
    }
}
