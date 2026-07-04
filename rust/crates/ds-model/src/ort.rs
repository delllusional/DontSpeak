//! onnxruntime runtime bootstrap: resolve the load-dynamic dylib, version-gate
//! it, and fetch the version-matched prebuilt (route A) — plus the optional
//! Windows CUDA GPU runtime. The SINGLE place the `ORT_DYLIB_PATH` env is set.

use std::path::{Path, PathBuf};

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
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        // Microsoft's official linux-aarch64 build — same .tgz layout as linux-x64.
        return Some(OrtDist {
            url: crate::urls::ONNXRUNTIME_DIST_URL,
            archive_sha256: crate::urls::ONNXRUNTIME_DIST_SHA256,
        });
    }
    // Intel macOS / other arches: no pinned dynamic dist → route B (build ort with
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
    let downloaded = model_path(onnxruntime_dylib_file());
    // Intel macOS is the one shipped platform with NO pinned dist to download (Microsoft
    // publishes arm64-only macOS archives since 1.2x) — fall back to a Homebrew-installed
    // runtime when nothing is downloaded. Resolved in ds-config so the loader and the
    // engine-usability gate share ONE source; returns `None` on platforms with a pinned dist,
    // where the SHA-pinned official bytes stay the source of truth.
    if !downloaded.as_deref().is_some_and(|p| p.is_file())
        && let Some(brew) = ds_config::brew_onnxruntime_dylib()
    {
        return Some(brew);
    }
    downloaded
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
pub fn is_onnxruntime_dylib_version_ok() -> bool {
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
    if !is_onnxruntime_dylib_version_ok() {
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
        && is_cuda_driver_present()
        && is_cuda_runtime_present()
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
        && is_cuda_driver_present()
        && is_cuda_runtime_present()
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
        // ort's builder methods return the builder INSIDE their error (for recovery), so chain
        // them with `?` in a closure that yields ort::Result and match on the whole GPU attempt.
        match (|| -> ort::Result<_> {
            let b = ort::session::Session::builder()?;
            // `.error_on_failure()`: WITHOUT it, ort registers the CUDA EP best-effort and returns
            // Ok even when registration SOFT-fails (device/provider unavailable), so the session
            // would silently commit on CPU while we returned `Cuda` — a mislabel. With it, a
            // registration failure propagates as `Err`, so the CPU fallback below fires AND the token
            // returned is honestly `Cpu`.
            Ok(b.with_execution_providers([CUDAExecutionProvider::default()
                .build()
                .error_on_failure()])?)
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

/// Sidecar marker written next to the DOWNLOADED onnxruntime dylib (under `model_dir()`)
/// holding the exact [`ONNXRUNTIME_VERSION`] that was fetched. Bare existence of the dylib
/// isn't enough to prove it's current: after a version bump, the naming convention macOS
/// embeds in the dylib's own Mach-O header (`libonnxruntime.1.dylib`) stays IDENTICAL across
/// every 1.x release (see `is_onnxruntime_dylib_version_ok`'s NAMING NOTE), so neither a bare
/// existence check nor that header scan can detect the bump on its own — a stale dylib would
/// never get automatically (or, since `ensure_onnxruntime_with_progress` is what a manual
/// re-download ultimately calls, even manually) re-fetched. This marker is the precise signal:
/// written only right after a successful extract, and checked BEFORE treating the downloaded
/// copy as up to date, so a version bump reliably triggers a re-fetch.
fn version_marker_path(dylib_path: &Path) -> PathBuf {
    let mut name = dylib_path.as_os_str().to_os_string();
    name.push(".ds-version");
    PathBuf::from(name)
}

/// True if `path` is a complete dylib WE downloaded+extracted AND its version marker
/// (see [`version_marker_path`]) matches the currently pinned [`ONNXRUNTIME_VERSION`] — the
/// gate [`ensure_onnxruntime_at`] uses to decide "already fetched" vs. "needs a (re)fetch".
/// Pure/local (no `model_dir()` lookup) so tests can drive it against a fixture path.
fn is_managed_download_up_to_date(path: &Path) -> bool {
    path.is_file()
        && std::fs::read_to_string(version_marker_path(path))
            .map(|s| s.trim() == ONNXRUNTIME_VERSION)
            .unwrap_or(false)
}

/// True if `path` is a complete AND CURRENT onnxruntime dylib we can use without re-fetching —
/// the gate [`ensure_onnxruntime_with_progress`] uses for its unlocked fast path. For the copy
/// WE download+extract (`model_path(onnxruntime_dylib_file())`) this means
/// [`is_managed_download_up_to_date`] (existence AND a matching version marker), so an
/// `ONNXRUNTIME_VERSION` bump correctly invalidates a stale on-disk dylib. A bundled
/// (`ORT_DYLIB_PATH` override, shipped signed+notarized with the app) or Homebrew-fallback
/// dylib isn't extracted by us and carries no marker, so existence alone remains the right
/// signal for those — this only applies the version gate to the path we actually manage.
pub(crate) fn is_downloaded_onnxruntime_up_to_date(path: &Path) -> bool {
    match model_path(onnxruntime_dylib_file()) {
        Some(managed) if managed == path => is_managed_download_up_to_date(path),
        _ => path.is_file(),
    }
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
    // Unlocked fast path: a version-matched managed download, or any bundled/brew dylib
    // (existence alone — see `is_downloaded_onnxruntime_up_to_date`), is treated as present. A
    // STALE managed download (marker missing/mismatched after an ONNXRUNTIME_VERSION bump)
    // falls through to the fetch below instead of being trusted forever.
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

/// Destination/source-explicit core of [`ensure_onnxruntime_with_progress`] (split out so
/// tests can drive it against a temp dir + a localhost archive without the real
/// `model_dir()`/pinned dist). Serialized per destination via `file_flight`, like
/// `ensure_at`: BOTH model setups pull this shared dylib, and with download targets
/// running in parallel the second request must ATTACH to the fetch in flight (block,
/// then find the file present below) instead of re-downloading the archive alongside it.
fn ensure_onnxruntime_at(
    final_path: &Path,
    url: &str,
    archive_sha256: &str,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let flight = crate::download::file_flight(final_path);
    let _in_flight = flight.lock().unwrap();
    // `final_path` IS the managed download target by contract (we're about to extract onto
    // it) — the version-marker gate applies unconditionally here, not the bundled/brew
    // fallback `is_downloaded_onnxruntime_up_to_date` uses (that distinction belongs to the
    // OUTER `ensure_onnxruntime_with_progress`, whose `final_path` might resolve to a
    // bundled/brew dylib we never reach this function for).
    if is_managed_download_up_to_date(final_path) {
        return Ok(());
    }
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
            download_to(url, tmp_tgz.path(), progress)?;
            if !verify_sha256(tmp_tgz.path(), archive_sha256) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "onnxruntime archive sha256 mismatch",
                ));
            }
            extract_runtime_member(tmp_tgz.path(), final_path)?;
            // Record the exact version just extracted so a LATER `ONNXRUNTIME_VERSION` bump
            // is detected even though the Mach-O header naming convention can't distinguish
            // it (see `version_marker_path`). Best-effort: a write failure here just means
            // the next run re-fetches unnecessarily — the dylib itself is already correct.
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
pub fn is_cuda_runtime_present() -> bool {
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
pub fn is_cuda_driver_present() -> bool {
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
    // One step per wheel — same download-to-temp / sha-verify / extract body and retry loop as
    // before, just wired through `run_download_set`'s per-step callback (real bytes) instead of
    // the old no-op `&|_, _| {}` + manual `progress(idx, total)` wheel-count bookkeeping.
    let steps: Vec<crate::setup::DownloadStep> = CUDA_WHEELS
        .iter()
        .map(|&(url, sha)| -> crate::setup::DownloadStep {
            let dir = dir.clone();
            Box::new(move |p: &dyn Fn(u64, u64)| -> std::io::Result<()> {
                let retries = DEFAULT_RETRIES.max(1);
                let mut last_err: Option<std::io::Error> = None;
                for attempt in 0..retries {
                    let tmp = tempfile::NamedTempFile::new_in(&dir)?;
                    let r = (|| -> std::io::Result<()> {
                        download_to(url, tmp.path(), p)?;
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
