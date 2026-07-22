//! onnxruntime runtime bootstrap: resolve the load-dynamic dylib, version-gate
//! it, and fetch the version-matched prebuilt (route A) — plus the optional
//! Windows CUDA GPU runtime. The SINGLE place the `ORT_DYLIB_PATH` env is set.

use std::path::{Path, PathBuf};

use crate::archive::extract_runtime_member;
use crate::download::{DEFAULT_RETRIES, DownloadState, download_to_with_state, is_permanent_error};
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
/// SHA-256 of the archive. See `onnxruntime_dist` for the per-target versions and the
/// api-level rationale.
pub(crate) struct OrtDist {
    pub(crate) url: &'static str,
    /// SHA-256 of the downloaded archive (`.tgz` on macOS, `.zip` on Windows).
    pub(crate) archive_sha256: &'static str,
}

/// The onnxruntime archive distribution for THIS target, or `None` on an
/// unsupported platform (the caller then documents route B / a manual dylib).
/// Pins are ONNX Runtime **1.27.1**, except Intel macOS on **1.23.2** — Microsoft's last
/// x86_64 macOS build. The workspace `ort` pin is api-23, the level that floor can serve, and
/// a NEWER runtime serves an older API request — `GetApi(23)` succeeds on a 1.27 dylib
/// (verified on-device). We moved OFF 1.24.2 because its model loader DEADLOCKS while
/// loading the SepFormer speaker-separation graph (the dictation speaker-lock); 1.27 loads
/// it in <1 s. Kokoro/Parakeet are unaffected (backward-compatible; on Apple Silicon they
/// run through MLX, not this dylib, anyway).
pub(crate) fn onnxruntime_dist() -> Option<OrtDist> {
    // Official Microsoft dynamic ORT only (pyke ships static `.a` — load-dynamic can't dlopen).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    // Intel macOS rides Microsoft's last x86_64 archive (1.23.2) — see the urls.rs pin.
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    // Intel macOS / other: no pin → route B (`download-binaries` or manual ORT_DYLIB_PATH).
    #[allow(unreachable_code)]
    None
}

/// The resolved path the onnxruntime dylib lives at, for the caller to set `ORT_DYLIB_PATH`.
///
/// Order: an externally supplied `ORT_DYLIB_PATH` pointing at a real file, then the copy the
/// macOS app bundles beside itself (signed + notarized with the app, so a build can ship a
/// runtime the target platform has no download for), then the managed download under
/// `model_dir()`, then a Homebrew install. `None` only if nothing resolves.
pub fn onnxruntime_dylib_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    if let Some(bundled) = bundled_onnxruntime_dylib() {
        return Some(bundled);
    }
    let downloaded = model_path(onnxruntime_dylib_file());
    // Last resort on Intel macOS, whose pinned dist stops at Microsoft's final x86_64 build:
    // a Homebrew runtime is newer and equally acceptable (version-gated like any other).
    if !downloaded.as_deref().is_some_and(|p| p.is_file())
        && let Some(brew) = ds_config::brew_onnxruntime_dylib()
    {
        return Some(brew);
    }
    downloaded
}

/// `Contents/Frameworks/libonnxruntime.dylib` of the running app bundle, when the executable
/// sits in one. Same shape as the MLX shim's bundled lookup; no signature check here because
/// this path only selects WHICH dylib `ort` dlopens, and a caller that can rewrite the bundle
/// can rewrite the executable next to it.
fn bundled_onnxruntime_dylib() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        let contents = executable
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))?
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))?;
        let dylib = contents.join("Frameworks/libonnxruntime.dylib");
        dylib.is_file().then_some(dylib)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The onnxruntime version the workspace `ort` pin (api-23) requires at runtime —
/// it's embedded in the dylib's `LC_ID_DYLIB` name (`libonnxruntime.<VER>.dylib`).
/// Defined in the download registry (`urls.rs`); re-exported here for the historical path.
pub use crate::urls::ONNXRUNTIME_VERSION;

#[cfg(any(target_os = "macos", test))]
fn has_supported_macos_dylib_id(mut reader: impl std::io::Read, expected_version: &str) -> bool {
    const MAX_MACHO_HEADER_BYTES: usize = 256 * 1024;
    const MAJOR_ID: &[u8] = b"libonnxruntime.1.dylib";
    let exact_id = format!("libonnxruntime.{expected_version}.dylib");

    let mut buf = vec![0u8; MAX_MACHO_HEADER_BYTES];
    let n = reader.read(&mut buf).unwrap_or(0);
    let header = &buf[..n];
    header
        .windows(MAJOR_ID.len())
        .any(|window| window == MAJOR_ID)
        || header
            .windows(exact_id.len())
            .any(|window| window == exact_id.as_bytes())
}

/// Cheap gate that the on-disk dylib matches what `ort` needs (status-poll safe).
///
/// Wrong-version dylib dlopens fine, but `GetApi(24)` NULL + ort rc.12 re-enters
/// `api()` OnceLock → self-deadlock (warm child hangs before READY). Reject before
/// `ort` loads. macOS accepts the major-only `LC_ID_DYLIB` used since 1.25 or the exact
/// target pin (Intel's last build is full-versioned 1.23.2). Exact bytes are archive-pinned.
pub fn is_onnxruntime_dylib_version_ok() -> bool {
    let Some(path) = onnxruntime_dylib_path() else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        let Ok(f) = std::fs::File::open(&path) else {
            return false;
        };
        has_supported_macos_dylib_id(f, ONNXRUNTIME_VERSION)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Managed SHA-pinned extract: presence implies version; no id string to scan.
        path.is_file()
    }
}

/// Single ORT bootstrap for in-process ONNX (Kokoro TTS + Parakeet STT): resolve,
/// version-gate, set `ORT_DYLIB_PATH`. Wrong version before `ort` = deadlock (see
/// [`is_onnxruntime_dylib_version_ok`]). Windows CUDA sets its own GPU dylib after.
pub fn ensure_ort_dylib() -> Result<PathBuf, String> {
    let path = onnxruntime_dylib_path().ok_or("cannot resolve onnxruntime dylib path")?;
    if !is_onnxruntime_dylib_version_ok() {
        return Err(format!(
            "onnxruntime dylib is not {ONNXRUNTIME_VERSION} — re-download it in Settings › Models"
        ));
    }
    set_ort_dylib_path(&path);
    Ok(path)
}

/// GPU-aware ORT bootstrap (Kokoro + Parakeet share one process / one runtime):
/// CUDA dylib when `want_gpu` and runtime present (Windows: set CUDA DLL dir once),
/// else [`ensure_ort_dylib`]. First loader wins; second reuses.
pub fn ensure_ort_dylib_gpu(want_gpu: bool) -> Result<PathBuf, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    if want_gpu
        && is_cuda_driver_present()
        && is_cuda_runtime_present()
        && let Some(gpu_dll) = cuda_onnxruntime_path()
    {
        if let Some(dir) = cuda_runtime_dir() {
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
            use windows::core::PCWSTR;

            static DLL_DIR: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
            DLL_DIR
                .get_or_init(|| {
                    let wide: Vec<u16> = dir
                        .as_os_str()
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    // SAFETY: `wide` is NUL-terminated and remains live for the duration of
                    // the call. The Win32 loader copies the directory into process state.
                    unsafe { SetDllDirectoryW(PCWSTR(wide.as_ptr())) }
                        .map_err(|e| format!("set CUDA DLL directory: {e}"))
                })
                .clone()?;
        }
        set_ort_dylib_path(&gpu_dll);
        return Ok(gpu_dll);
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if want_gpu
        && is_cuda_driver_present()
        && is_cuda_runtime_present()
        && let Some(gpu_so) = cuda_onnxruntime_path()
    {
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
                // SAFETY: `c` NUL-terminated for the call; never dlclose — keep RTLD_GLOBAL
                // symbols resident for ORT's later provider dlopen (RTLD_NOW = deps resolved).
                let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
                h.is_null() // retry next pass on failure
            });
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }
    });
}

/// Shared CUDA session builder for ONNX TTS + Parakeet STT (single EP + CPU fallback).
///
/// Attempts CUDA only when `want_gpu` and runtime + driver present (same gate as
/// [`ensure_ort_dylib_gpu`]). Static preference alone would report `Cuda` on CPU-only
/// boxes; gated, `Cuda` means EP registered. EP fail with runtime present logs real ort
/// error then falls back. Off Windows/Linux-x64: CPU only.
pub fn cuda_session_builder(
    want_gpu: bool,
) -> Result<
    (
        ort::session::builder::SessionBuilder,
        ds_config::RealizedProvider,
    ),
    String,
> {
    use ds_config::RealizedProvider;
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    if want_gpu && is_cuda_runtime_present() && is_cuda_driver_present() {
        use ort::execution_providers::CUDAExecutionProvider;
        // Chain builders in a closure: ort returns the builder inside Err for recovery.
        match (|| -> ort::Result<_> {
            let b = ort::session::Session::builder()?;
            // `error_on_failure`: soft EP fail would Ok+CPU while we return `Cuda`.
            Ok(b.with_execution_providers([CUDAExecutionProvider::default()
                .build()
                .error_on_failure()])?)
        })() {
            Ok(b) => return Ok((b, RealizedProvider::Cuda)),
            Err(e) => {
                log::warn!(
                    target: "model",
                    "CUDA EP registration failed — running on CPU: {e}"
                )
            }
        }
    }
    let _ = want_gpu;
    let b = ort::session::Session::builder().map_err(|e| format!("ort session builder: {e}"))?;
    Ok((b, RealizedProvider::Cpu))
}

/// Sole writer of `ORT_DYLIB_PATH` (CPU path + Windows CUDA). Call before first session.
pub fn set_ort_dylib_path(path: &std::path::Path) {
    // Once: TTS/STT warm in parallel; concurrent `set_var` + ort's lazy read is UB.
    // Path is deterministic per process — first-wins.
    static DYLIB_ONCE: std::sync::Once = std::sync::Once::new();
    DYLIB_ONCE.call_once(|| {
        // SAFETY: Once = single writer; ort reads ORT_DYLIB_PATH lazily on first session.
        unsafe { std::env::set_var("ORT_DYLIB_PATH", path) };
    });
}

/// Sidecar next to managed download: exact [`ONNXRUNTIME_VERSION`]. Mach-O major-only
/// id is identical across 1.x, so existence/header alone never re-fetch after a pin bump.
fn version_marker_path(dylib_path: &Path) -> PathBuf {
    let mut name = dylib_path.as_os_str().to_os_string();
    name.push(".ds-version");
    PathBuf::from(name)
}

/// Managed extract + matching [`version_marker_path`] pin (fixture-friendly; no model_dir).
fn is_managed_download_up_to_date(path: &Path) -> bool {
    path.is_file()
        && std::fs::read_to_string(version_marker_path(path))
            .map(|s| s.trim() == ONNXRUNTIME_VERSION)
            .unwrap_or(false)
}

/// Fast-path "use this dylib": managed path needs version marker; bundled/brew = existence only.
pub(crate) fn is_downloaded_onnxruntime_up_to_date(path: &Path) -> bool {
    match model_path(onnxruntime_dylib_file()) {
        Some(managed) if managed == path => is_managed_download_up_to_date(path),
        _ => path.is_file(),
    }
}

/// Ensure the onnxruntime dylib exists locally under `model_dir()` (route A),
/// reporting `.tgz` download progress. If already present, returns its path.
/// Otherwise downloads the version-matched `.tgz` to a temp file (verifying its
/// pinned SHA-256), extracts the single `libonnxruntime*.dylib` (/.so/.dll)
/// member, and atomically renames it onto the final dylib path. Returns an
/// `Unsupported` error on a platform with no pinned distribution (the README
/// documents route B there).
pub fn ensure_onnxruntime_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let final_path = onnxruntime_dylib_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    // Stale managed marker falls through to re-fetch.
    if is_downloaded_onnxruntime_up_to_date(&final_path) {
        return Ok(final_path);
    }
    let Some(dist) = onnxruntime_dist() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no pinned onnxruntime distribution for this platform; \
             install it with Homebrew (`brew install onnxruntime`) on Intel macOS, \
             build ort with --features download-binaries (route B), or set \
             ORT_DYLIB_PATH to a manually installed libonnxruntime",
        ));
    };
    ensure_onnxruntime_at(&final_path, dist.url, dist.archive_sha256, progress)?;
    Ok(final_path)
}

/// Testable core of [`ensure_onnxruntime_with_progress`]. `file_flight` so parallel
/// Kokoro/Parakeet setups attach to one in-flight extract.
fn ensure_onnxruntime_at(
    final_path: &Path,
    url: &str,
    archive_sha256: &str,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let flight = crate::download::file_flight(final_path);
    let _in_flight = flight.lock().unwrap();
    // Managed extract only here (outer path may be bundled/brew).
    if is_managed_download_up_to_date(final_path) {
        return Ok(());
    }
    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("dylib path has no parent"))?;
    std::fs::create_dir_all(dir)?;

    // Same retry policy as model files (transient vs permanent).
    let retries = DEFAULT_RETRIES.max(1);
    let tmp_tgz = tempfile::NamedTempFile::new_in(dir)?;
    let mut state = DownloadState::default();
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries {
        let result = (|| -> std::io::Result<()> {
            download_to_with_state(url, tmp_tgz.path(), progress, &mut state)?;
            if !verify_sha256(tmp_tgz.path(), archive_sha256) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "onnxruntime archive sha256 mismatch",
                ));
            }
            extract_runtime_member(tmp_tgz.path(), final_path)?;
            // Best-effort marker; write fail only causes a later re-fetch.
            let _ = std::fs::write(version_marker_path(final_path), ONNXRUNTIME_VERSION);
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(()),
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
// Kokoro TTS and Parakeet STT can run on an NVIDIA GPU (2.8-4.6x faster than
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

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn cuda_version_marker(dir: &Path) -> PathBuf {
    dir.join(".dontspeak-cuda-runtime")
}

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn cuda_version_fingerprint() -> String {
    CUDA_WHEELS
        .iter()
        .map(|(url, sha)| format!("{url} {sha}"))
        .collect::<Vec<_>>()
        .join("\n")
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
pub fn is_cuda_runtime_present() -> bool {
    let Some(dir) = cuda_runtime_dir() else {
        return false;
    };
    if !std::fs::read_to_string(cuda_version_marker(&dir))
        .is_ok_and(|s| s == cuda_version_fingerprint())
    {
        return false;
    }
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
pub fn is_cuda_driver_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        // dlopen the driver lib through the loader's normal search path; a non-null handle ⇒ the
        // NVIDIA driver is installed. Close it again — this is only a presence probe.
        let name = c"libcuda.so.1";
        // SAFETY: dlopen gets a NUL-terminated `&CStr` literal; the returned handle is
        // null-checked before use.
        let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if h.is_null() {
            false
        } else {
            // SAFETY: `h` was just returned non-null by dlopen; dlclose releases that
            // same handle exactly once.
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
        // SAFETY: LoadLibraryW gets a static NUL-terminated wide literal (`w!`) and
        // returns an owned module handle; nvcuda.dll is the system's NVIDIA driver stub,
        // loaded here only as a presence probe and freed right below.
        match unsafe { LoadLibraryW(w!("nvcuda.dll")) } {
            Ok(h) => {
                // SAFETY: `h` is the live module handle LoadLibraryW just returned Ok;
                // freed exactly once.
                let _ = unsafe { FreeLibrary(h) };
                true
            }
            Err(_) => false,
        }
    }
}

/// Download + extract the pinned CUDA runtime wheels into [`cuda_runtime_dir`]. Each wheel is
/// a zip; we pull out every `*.dll`/`*.so`. Idempotent (a present runtime returns immediately).
///
/// Reports REAL byte-level progress across the WHOLE wheel set via the shared
/// `crate::setup::run_download_set` aggregator (the same one the Kokoro/Parakeet/SepFormer
/// asset sets use) — one `crate::setup::DownloadStep` per wheel, `total` = the exact summed
/// `Content-Length` of every pinned wheel (`crate::urls::CUDA_WHEEL_SIZES`), rather than the
/// old wheel-COUNT bookkeeping (`progress(idx, total_wheels)`), which made the CUDA row's ring
/// jump in big uneven steps instead of climbing smoothly with the actual bytes. ~1.4 GB on
/// first fetch — the caller (GPU opt-in) gates this.
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
    if is_cuda_runtime_present() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir)?;
    let total: u64 = crate::urls::CUDA_WHEEL_SIZES.iter().sum();
    // One step per wheel through `run_download_set` (bounded parallel pool). Each extracted DLL/SO
    // lands via an atomic temp→rename, so concurrent workers never expose a torn file. The pinned
    // wheels carry disjoint basenames; a cross-wheel name clash would be last-writer-wins here (a
    // benign redistributable duplicate), not corruption.
    let steps: Vec<crate::setup::DownloadStep> = CUDA_WHEELS
        .iter()
        .map(|&(url, sha)| -> crate::setup::DownloadStep {
            let dir = dir.clone();
            Box::new(move |p: &dyn Fn(u64, u64)| -> std::io::Result<()> {
                let retries = DEFAULT_RETRIES.max(1);
                let tmp = tempfile::NamedTempFile::new_in(&dir)?;
                let mut state = DownloadState::default();
                let mut last_err: Option<std::io::Error> = None;
                for attempt in 0..retries {
                    let r = (|| -> std::io::Result<()> {
                        download_to_with_state(url, tmp.path(), p, &mut state)?;
                        if !verify_sha256(tmp.path(), sha) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "cuda wheel sha256 mismatch",
                            ));
                        }
                        extract_libs(tmp.path(), &dir)
                    })();
                    match r {
                        Ok(()) => return Ok(()),
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
                Err(last_err
                    .unwrap_or_else(|| std::io::Error::other("cuda runtime download failed")))
            })
        })
        .collect();
    crate::setup::run_download_set(progress, total, steps)?;
    std::fs::write(cuda_version_marker(&dir), cuda_version_fingerprint())?;
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

    #[test]
    fn macos_dylib_id_accepts_major_or_exact_pin_only() {
        assert!(has_supported_macos_dylib_id(
            &b"header libonnxruntime.1.dylib trailer"[..],
            "1.27.1",
        ));
        assert!(has_supported_macos_dylib_id(
            &b"header libonnxruntime.1.23.2.dylib trailer"[..],
            "1.23.2",
        ));
        assert!(!has_supported_macos_dylib_id(
            &b"header libonnxruntime.1.24.2.dylib trailer"[..],
            "1.23.2",
        ));
    }

    #[test]
    fn macos_dylib_id_scan_reaches_beyond_the_old_64k_limit() {
        let mut header = vec![0u8; 96 * 1024];
        header.extend_from_slice(b"libonnxruntime.1.dylib");
        assert!(has_supported_macos_dylib_id(
            std::io::Cursor::new(header),
            "1.27.1",
        ));
    }

    /// A minimal valid runtime archive holding one dylib member, in THIS platform's real
    /// format (`.zip` with `lib/onnxruntime.dll` on Windows, `.tgz` with a `lib/*.so`
    /// member elsewhere) — so the test drives the SAME `extract_runtime_member` arm
    /// production uses.
    #[cfg(target_os = "windows")]
    fn fixture_runtime_archive(member: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file(
            "lib/onnxruntime.dll",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        w.write_all(member).unwrap();
        w.finish().unwrap().into_inner()
    }
    #[cfg(not(target_os = "windows"))]
    fn fixture_runtime_archive(member: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            let mut h = tar::Header::new_gnu();
            h.set_size(member.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, "lib/libonnxruntime.so", member)
                .unwrap();
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    /// End-to-end over an `httpmock` server: two concurrent `ensure_onnxruntime_at` calls for
    /// the SAME destination make exactly ONE network fetch (the second blocks on the per-path
    /// flight lock, then finds the extracted dylib present and attaches), and the archive
    /// member lands extracted + atomically renamed onto the destination.
    #[test]
    fn concurrent_onnxruntime_ensure_attaches_and_extracts() {
        let member = b"fake onnxruntime dylib bytes".to_vec();
        let archive = fixture_runtime_archive(&member);
        let sha = crate::hash::sha256_hex(&archive);

        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/onnxruntime-dist.archive");
            then.status(200).body(archive.clone());
        });
        let url = server.url("/onnxruntime-dist.archive");

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("libonnxruntime.fixture");
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let url = url.clone();
                let sha = sha.clone();
                let dest = dest.clone();
                std::thread::spawn(move || ensure_onnxruntime_at(&dest, &url, &sha, &|_, _| {}))
            })
            .collect();
        for t in threads {
            t.join().unwrap().expect("both callers succeed");
        }
        mock.assert_calls(1);
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            member,
            "the dylib member is extracted onto the destination"
        );
    }

    #[test]
    fn is_managed_download_up_to_date_requires_a_matching_version_marker() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("libonnxruntime.fixture");

        // Missing entirely → not up to date.
        assert!(!is_managed_download_up_to_date(&dest));

        // File present but NO marker written yet (e.g. an old on-disk copy from before this
        // fix shipped) → still not up to date, so it gets re-fetched once.
        std::fs::write(&dest, b"some dylib bytes").unwrap();
        assert!(!is_managed_download_up_to_date(&dest));

        // Marker present but for a DIFFERENT version (the exact post-bump scenario this fix
        // targets) → not up to date.
        std::fs::write(version_marker_path(&dest), "0.0.0-not-the-real-version").unwrap();
        assert!(!is_managed_download_up_to_date(&dest));

        // Marker matches the currently pinned version → up to date.
        std::fs::write(version_marker_path(&dest), ONNXRUNTIME_VERSION).unwrap();
        assert!(is_managed_download_up_to_date(&dest));
    }

    /// The scenario the finding describes: a stale dylib already on disk (simulated here by a
    /// version marker that doesn't match the CURRENT pin) must be automatically re-fetched by
    /// `ensure_onnxruntime_at` — not silently kept forever just because a file already exists
    /// at the destination.
    #[test]
    fn ensure_onnxruntime_at_refetches_when_the_version_marker_is_stale() {
        let member = b"the CURRENT-version dylib bytes".to_vec();
        let archive = fixture_runtime_archive(&member);
        let sha = crate::hash::sha256_hex(&archive);

        // One mock, hit twice: once for the initial populate, once for the re-fetch after
        // the version marker goes stale — `httpmock` mocks aren't single-use by default.
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/onnxruntime-dist.archive");
            then.status(200).body(archive.clone());
        });
        let url = server.url("/onnxruntime-dist.archive");

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("libonnxruntime.fixture");

        // First call: nothing on disk yet → fetches + extracts + writes the current marker.
        ensure_onnxruntime_at(&dest, &url, &sha, &|_, _| {}).expect("first fetch succeeds");
        mock.assert_calls(1);
        assert!(is_managed_download_up_to_date(&dest));

        // Simulate an ONNXRUNTIME_VERSION bump: the dylib on disk is now considered stale even
        // though the file itself hasn't changed — exactly what happens after a version pin
        // bump ships while an old dylib is still sitting in model_dir().
        std::fs::write(version_marker_path(&dest), "9.9.9-stale").unwrap();
        assert!(!is_managed_download_up_to_date(&dest));

        // Second call must NOT treat the stale file as present — it re-fetches (a SECOND
        // server hit) and rewrites the marker back to the current version.
        ensure_onnxruntime_at(&dest, &url, &sha, &|_, _| {})
            .expect("stale marker triggers a re-fetch, which succeeds");
        mock.assert_calls(2);
        assert!(is_managed_download_up_to_date(&dest));
        assert_eq!(std::fs::read(&dest).unwrap(), member);
    }
}
