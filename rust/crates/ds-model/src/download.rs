//! Download engine — atomic temp + rename, retry with backoff, sha-verify, and
//! the installer prefetch fast-path. Blocking `attohttpc` (no tokio); a
//! socket-level per-read inactivity timeout aborts a stalled CDN.
//!
//! Retry classification rides on `io::ErrorKind`, not a custom error enum:
//! `InvalidData` = checksum mismatch and `NotFound` = HTTP 4xx or definitive DNS
//! name failure (permanent, fail fast); `TimedOut` = other transport/5xx/truncation
//! failures (transient, retried) — see `is_permanent_error`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::hash::verify_sha256;
use crate::model_path;
use crate::spec::ModelSpec;

/// Default download retry count (full re-download; Range-resume deferred).
pub(crate) const DEFAULT_RETRIES: u32 = 3;

/// Stall guards so a wedged CDN never hangs the caller (engine tick / GUI). A
/// connect timeout + a per-read INACTIVITY timeout, NOT a whole-request timeout:
/// a 150 MB–1.5 GB model can legitimately take minutes. `attohttpc`'s
/// `read_timeout` sets the socket `SO_RCVTIMEO`, so it fires only when NO bytes
/// arrive within the window — a slow-but-progressing download survives while a
/// truly stalled socket aborts and the retry loop kicks in.
// Connect stays short — it only catches an unreachable host (byte transfer is
// bounded by the per-read timeout). 8s fails fast on a dead host without flapping
// on a briefly-slow DNS/TLS handshake.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The OS trust store, loaded ONCE, as attohttpc root certs. attohttpc's
/// `tls-rustls-webpki-roots-ring` feature pulls the webpki-roots crate but NOT the feature
/// flag its root-loading code is gated on, so its built-in store is EMPTY — every HTTPS GET
/// would fail "no root trust anchors". We inject the OS roots ourselves; the rustls impl adds
/// `add_root_certificate` entries to the store regardless of that broken cfg.
fn os_root_certs() -> &'static [rustls_pki_types::CertificateDer<'static>] {
    use std::sync::OnceLock;
    static ROOTS: OnceLock<Vec<rustls_pki_types::CertificateDer<'static>>> = OnceLock::new();
    ROOTS.get_or_init(|| rustls_native_certs::load_native_certs().certs)
}

/// A GET builder with our stall-guard timeouts AND the OS trust roots injected — the SINGLE
/// place every HTTPS download (ONNX assets, the ORT dylib, and the Core ML repos) is set up.
pub(crate) fn http_get_builder(url: &str) -> attohttpc::RequestBuilder {
    let mut rb = attohttpc::get(url)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT);
    for cert in os_root_certs() {
        rb = rb.add_root_certificate(cert.clone());
    }
    rb
}

// ─────────────────────────────────────────────────────────────────────────────
// Installer prefetch source: when set, the low-level GET helpers COPY from a dir
// of locally pre-downloaded files (keyed by URL basename) instead of hitting the
// network. The installer fetches the assets itself and points this at its temp
// dir; the verify + extract logic below is reused UNCHANGED. Unset in the normal
// app/engine path.
// ─────────────────────────────────────────────────────────────────────────────
static PREFETCH_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Point downloads at a dir of pre-fetched files (or `None` to disable). Files are
/// matched by [`url_basename`]. Used by `ds-helper --install-prefetched`.
pub fn set_prefetch_source(dir: Option<PathBuf>) {
    *PREFETCH_DIR.lock().unwrap() = dir;
}

/// The last path segment of `url` (query/fragment stripped) — the name a prefetched
/// file is expected under, and the name the installer saves each download as.
pub fn url_basename(url: &str) -> &str {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let trimmed = no_query.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// If a prefetch dir is set and holds `url`'s file, return its path (else `None`).
fn prefetch_local(url: &str) -> Option<PathBuf> {
    let guard = PREFETCH_DIR.lock().unwrap();
    let dir = guard.as_ref()?;
    let p = dir.join(url_basename(url));
    p.is_file().then_some(p)
}

// ─────────────────────────────────────────────────────────────────────────────
// In-flight file registry — ONE download per destination path, attach semantics.
// Download targets now run in PARALLEL (each in its own thread), and two targets
// can need the SAME file: both model setups ensure the shared onnxruntime dylib,
// and the full-Kokoro set contains the frontend assets that `kokoro_frontend` also
// fetches alone. The registry hands out one lock per final path; a second caller
// blocks on it (attaching to the fetch already in flight), then re-checks
// presence under the lock and returns without re-downloading. Entries are tiny
// (a handful of model paths) and live for the process — no cleanup needed.
// ─────────────────────────────────────────────────────────────────────────────
pub(crate) fn file_flight(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static FLIGHTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    FLIGHTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}

/// Copy a prefetched file onto `dest` and report it as a completed transfer (so the
/// caller's progress UI jumps to 100% for an instant local copy). Shared by the two
/// download fns' installer fast-paths.
fn copy_prefetched(local: &Path, dest: &Path, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    if local.metadata()?.len() > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "prefetched file exceeds download size limit",
        ));
    }
    std::fs::copy(local, dest)?;
    let len = dest.metadata().map(|m| m.len()).unwrap_or(0);
    progress(len, len);
    Ok(())
}

/// Ensure `spec`'s file exists locally and matches its SHA-256, downloading it
/// if needed. Returns the final path on success.
///
/// Flow (§D, Range-resume deferred): if the final path already verifies, return
/// it. Otherwise GET the URL into a sibling `.part` temp file (up to N retries),
/// verify the `.part`'s SHA-256, then atomically persist (rename) it onto the
/// final path. A failed verify deletes the `.part` and retries with a full
/// re-download.
pub fn ensure(spec: &ModelSpec) -> std::io::Result<PathBuf> {
    ensure_with_retries(spec, DEFAULT_RETRIES, &|_, _| {})
}

/// Like [`ensure`] but reports `(downloaded_bytes, total_bytes)` during the fetch.
pub fn ensure_with_progress(
    spec: &ModelSpec,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    ensure_with_retries(spec, DEFAULT_RETRIES, progress)
}

fn ensure_with_retries(
    spec: &ModelSpec,
    retries: u32,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let final_path = model_path(&spec.file_name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve model_dir() (no data dir)",
        )
    })?;
    ensure_at(&final_path, spec, retries, progress)?;
    Ok(final_path)
}

/// The destination-explicit core of [`ensure_with_retries`] (split out so tests can
/// drive it against a temp dir without touching the real `model_dir()`). Serialized
/// per destination via [`file_flight`]: with download targets running in parallel, a
/// concurrent request for the SAME file blocks here, then finds the file present at
/// the verify below and returns — it ATTACHES to the finished fetch instead of
/// re-downloading (or corrupting) it.
fn ensure_at(
    final_path: &Path,
    spec: &ModelSpec,
    retries: u32,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    // Best-effort, once-per-process orphan sweep (see `sweep_orphans_once`) — run FIRST, on
    // every call (not just ones that end up downloading), since a status-only check that finds
    // everything already present is by far the most common call in a normal run.
    if let Some(dir) = final_path.parent() {
        sweep_orphans_once(dir);
    }

    let flight = file_flight(final_path);
    let _in_flight = flight.lock().unwrap();

    if verify_sha256(final_path, &spec.sha256) {
        return Ok(());
    }

    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    std::fs::create_dir_all(dir)?;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries.max(1) {
        match download_once(&spec.url, dir, final_path, &spec.sha256, progress) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Fast-fail permanent errors (checksum mismatch, HTTP 404): a
                // retry would only re-fetch the same wrong/absent body — for a
                // 150 MB+ model that is minutes of wasted bandwidth. Only
                // transient errors (timeout, reset, 5xx) are worth retrying.
                if is_permanent_error(&e) {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("permanent download failure (not retried): {e}"),
                    ));
                }
                last_err = Some(std::io::Error::new(
                    e.kind(),
                    format!("attempt {} of {}: {e}", attempt + 1, retries.max(1)),
                ));
                // Brief backoff before the next attempt so a momentary network
                // hiccup has time to clear (skip after the final attempt).
                if attempt + 1 < retries.max(1) {
                    std::thread::sleep(std::time::Duration::from_millis(
                        500 * (attempt as u64 + 1),
                    ));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("download failed")))
}

/// Map an HTTP status code to an `io::Error` whose `kind()` encodes whether the
/// failure is PERMANENT (don't retry) or TRANSIENT (retry). A 4xx status (e.g.
/// 404 — the file was re-hosted/removed) is permanent and surfaces as `NotFound`;
/// any other non-success status (5xx) is transient and surfaces as `TimedOut`.
fn classify_http_status(code: u16) -> std::io::Error {
    if (400..500).contains(&code) {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("permanent HTTP {code}"),
        )
    } else {
        std::io::Error::new(std::io::ErrorKind::TimedOut, format!("HTTP {code}"))
    }
}

fn is_permanent_dns_error(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::NotFound {
        return true;
    }

    // `ToSocketAddrs` preserves resolver error codes that `io::ErrorKind` does not
    // categorize. Limit this to the supported OSes' definitive "name does not exist"
    // results; temporary resolver failures (EAI_AGAIN / WSATRY_AGAIN) remain retryable.
    match e.raw_os_error() {
        #[cfg(target_os = "windows")]
        Some(11001 | 11003 | 11004) => true,
        #[cfg(target_os = "linux")]
        Some(-2 | -5) => true,
        #[cfg(target_os = "macos")]
        Some(7 | 8) => true,
        _ => false,
    }
}

/// Transport failures are transient except for a definitive DNS "name not found"
/// result, which cannot recover across this download's short retry window.
fn transport_err(e: attohttpc::Error) -> std::io::Error {
    let kind = match e.kind() {
        attohttpc::ErrorKind::Io(source) if is_permanent_dns_error(source) => {
            std::io::ErrorKind::NotFound
        }
        _ => std::io::ErrorKind::TimedOut,
    };
    std::io::Error::new(kind, e.to_string())
}

/// Open a GET body stream: returns the body reader + the `Content-Length` (0 if
/// absent). `attohttpc`'s `read_timeout` is a SOCKET-level per-read timeout, so a
/// stalled CDN aborts mid-download while a slow-but-progressing large model keeps
/// going. Non-2xx status is classified (4xx permanent / 5xx transient); transport
/// errors are transient.
fn http_get_stream(url: &str) -> std::io::Result<(attohttpc::ResponseReader, u64)> {
    let resp = http_get_builder(url).send().map_err(transport_err)?;
    if !resp.is_success() {
        return Err(classify_http_status(resp.status().as_u16()));
    }
    let (_status, headers, reader) = resp.split();
    let total: u64 = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((reader, total))
}

/// Whether an error from one download attempt is PERMANENT — retrying it would
/// only waste time + bandwidth re-fetching a (possibly huge) body. A checksum
/// mismatch (`InvalidData`) means the bytes that arrived are wrong (re-host, MITM,
/// or a stale pinned digest) and a 4xx (`NotFound`) means the URL is gone; both
/// fail fast. Everything else (timeout, reset, 5xx) is transient and retried.
pub(crate) fn is_permanent_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::NotFound
    )
}

/// One download attempt: GET → `.part` → verify → atomic rename. The temp file
/// is cleaned up automatically on any early return (NamedTempFile drops).
fn download_once(
    url: &str,
    dir: &Path,
    final_path: &Path,
    expected_sha: &str,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;

    // Installer path: a pre-downloaded copy exists locally — use it if it verifies.
    // If the local copy is CORRUPT, fall through to a normal network fetch rather than
    // failing the whole install on a bad temp blob (the installer's {tmp} download could
    // be partial/damaged; the real bytes still download fine).
    if let Some(local) = prefetch_local(url) {
        copy_prefetched(&local, tmp.path(), progress)?;
        if verify_sha256(tmp.path(), expected_sha) {
            tmp.persist(final_path).map_err(|e| e.error)?;
            return Ok(());
        }
        // Discard the bad copy and start the network path with a clean temp file.
        tmp = tempfile::NamedTempFile::new_in(dir)?;
    }

    // Per-read inactivity + connect timeouts (see CONNECT_TIMEOUT / READ_TIMEOUT):
    // a stalled CDN aborts instead of hanging the caller indefinitely.
    let (mut reader, total) = http_get_stream(url)?;
    if total > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "download Content-Length exceeds size limit",
        ));
    }
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut next_emit: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        tmp.write_all(&buf[..n])?;
        downloaded += n as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "download exceeds size limit",
            ));
        }
        // Throttle progress to ~1 MB steps to bound UI callbacks.
        if downloaded >= next_emit {
            progress(downloaded, total);
            next_emit = downloaded + 1_048_576;
        }
    }
    tmp.flush()?;
    progress(downloaded, total.max(downloaded)); // final 100%

    // TRUNCATION (transient): the CDN closed the stream early — `read` returns 0
    // (clean EOF) with no error, so the body is short. This is a network hiccup,
    // NOT corrupt bytes, so surface it as TimedOut so the retry loop RE-FETCHES it
    // (otherwise the short `.part` fails the sha check below and is mis-classified
    // as a permanent InvalidData, forcing the user to re-click — the reported
    // "succeeds on the 2nd/3rd attempt" symptom). Only checkable when the server
    // sent a Content-Length.
    if total > 0 && downloaded < total {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("truncated download: got {downloaded} of {total} bytes"),
        ));
    }

    // Verify the .part BEFORE renaming so a corrupt body never lands as final. A
    // mismatch on a COMPLETE body (downloaded == total, or length unknown) is a
    // genuine corrupt/stale-digest case → permanent (InvalidData), not retried.
    if !verify_sha256(tmp.path(), expected_sha) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sha256 mismatch on downloaded file",
        ));
    }

    // Atomic rename onto the final path.
    tmp.persist(final_path).map_err(|e| e.error)?;
    Ok(())
}

/// GET `url` straight into `dest` (no checksum here; caller verifies). Used by
/// the onnxruntime `.tgz` download (the dylib is extracted + verified separately
/// via the archive digest).
pub(crate) fn download_to(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    // Installer path: copy the pre-downloaded archive (the caller verifies its sha).
    if let Some(local) = prefetch_local(url) {
        return copy_prefetched(&local, dest, progress);
    }
    let (mut reader, total) = http_get_stream(url)?;
    if total > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "download Content-Length exceeds size limit",
        ));
    }
    let mut f = std::fs::File::create(dest)?;
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut next_emit: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
        downloaded += n as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "download exceeds size limit",
            ));
        }
        if downloaded >= next_emit {
            progress(downloaded, total);
            next_emit = downloaded + 1_048_576;
        }
    }
    f.flush()?;
    progress(downloaded, total.max(downloaded));
    // Same truncation guard as download_once: a short body (CDN closed early) is
    // TRANSIENT, so the caller's retry loop re-fetches instead of failing on the
    // downstream sha check.
    if total > 0 && downloaded < total {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("truncated download: got {downloaded} of {total} bytes"),
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Orphaned partial-download temp-file sweep.
//
// Every downloader in this crate (this module's `download_once`, `ort::ensure_onnxruntime_at`,
// `coreml_repo::download_one`) writes into a `tempfile`-created file in the SAME directory as
// its final destination, using `tempfile`'s DEFAULT naming (prefix `.tmp`, no suffix — none of
// them override it), then atomically renames it onto the final path on success. On a normal
// return (success OR error) the temp file's `Drop` deletes it — but `Drop` never runs on
// SIGKILL / a force-quit / a power loss, so a run killed mid-download can leave a `.tmp*` file
// behind that nothing else ever looks at again; over time these accumulate indefinitely.
// There's no dedicated "app startup" hook in this crate to wire a sweep into (the eager
// pre-download orchestrators live in a sibling module), so instead we sweep once per process
// the first time any file-based download is requested (see `sweep_orphans_once`, called from
// `ensure_at`) — a "sensible point" that fires on essentially every normal run.
// ─────────────────────────────────────────────────────────────────────────────

/// Only a temp file at least this old is swept — a `.tmp*` file could be a genuinely in-flight
/// download (from THIS process a moment before its own rename, or another concurrently running
/// instance); only a leftover this old is safe to assume abandoned by a prior, uncleanly-ended
/// run.
const MIN_ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

static ORPHAN_SWEEP_ONCE: std::sync::Once = std::sync::Once::new();

/// Run [`sweep_orphaned_temp_files`] against `dir` exactly once per process. `dir` is
/// `model_dir()` for every real caller (every [`ensure_at`] request resolves its destination
/// under it), so one sweep from there also reaches the Core ML / CUDA subdirectories nested
/// under it — the other two downloaders' temp files included.
fn sweep_orphans_once(dir: &Path) {
    ORPHAN_SWEEP_ONCE.call_once(|| sweep_orphaned_temp_files(dir));
}

/// Recursively remove orphaned partial-download temp files under `dir`: entries whose name
/// matches `tempfile`'s default naming AND are older than [`MIN_ORPHAN_AGE`]. Best-effort — a
/// removal failure (permissions, a racing delete) is silently skipped; this is opportunistic
/// cleanup, not a correctness requirement, so a missed orphan simply waits for the next sweep.
pub(crate) fn sweep_orphaned_temp_files(dir: &Path) {
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => sweep_orphaned_temp_files(&path),
            Ok(ft) if ft.is_file() => {
                let is_tmp = entry
                    .file_name()
                    .to_str()
                    .map(|n| n.starts_with(".tmp"))
                    .unwrap_or(false);
                if !is_tmp {
                    continue;
                }
                let old_enough = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .map(|age| age >= MIN_ORPHAN_AGE)
                    .unwrap_or(false);
                if old_enough {
                    let _ = std::fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Localhost happy-path: serve a known body over an `httpmock` server and exercise
    /// the temp+rename+verify path of `ensure`'s inner `download_once` WITHOUT a
    /// real CDN. We call `download_once` directly so we control the dir and avoid
    /// touching the user's real model_dir.
    #[test]
    fn download_once_happy_path_over_localhost() {
        let body = b"hello dontspeak model fixture".to_vec();
        let sha = crate::hash::sha256_hex(&body);

        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/model.bin");
            then.status(200).body(body.clone());
        });
        let url = server.url("/model.bin");

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("model.bin");
        download_once(&url, dir.path(), &final_path, &sha, &|_, _| {})
            .expect("download_once should succeed and verify");
        mock.assert();

        assert!(final_path.is_file(), "final file persisted");
        assert_eq!(std::fs::read(&final_path).unwrap(), body);
        // No leftover .part / temp file in the dir besides the final.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["model.bin".to_string()]);
    }

    /// A wrong checksum makes `download_once` reject and leave NO final file.
    #[test]
    fn download_once_rejects_bad_checksum() {
        let body = b"corrupt".to_vec();
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/m.bin");
            then.status(200).body(body.clone());
        });
        let url = server.url("/m.bin");
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("m.bin");
        let err = download_once(&url, dir.path(), &final_path, "deadbeef", &|_, _| {}).unwrap_err();
        mock.assert();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!final_path.exists(), "no final file on checksum mismatch");
    }

    /// Two concurrent `ensure_at` calls for the SAME destination must produce ONE
    /// network fetch: the second blocks on the per-path flight lock, then finds the
    /// verified file present and attaches (returns Ok without downloading) —
    /// `mock.assert_calls(1)` would fail if both fetched.
    #[test]
    fn concurrent_ensure_of_same_file_attaches_instead_of_refetching() {
        let body = b"shared asset fetched exactly once".to_vec();
        let sha = crate::hash::sha256_hex(&body);

        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/shared.bin");
            then.status(200).body(body.clone());
        });
        let url = server.url("/shared.bin");

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("shared.bin");
        let spec = ModelSpec {
            file_name: "shared.bin".to_string(),
            url,
            sha256: sha,
        };
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let spec = spec.clone();
                let final_path = final_path.clone();
                std::thread::spawn(move || ensure_at(&final_path, &spec, 1, &|_, _| {}))
            })
            .collect();
        for t in threads {
            t.join().unwrap().expect("both callers succeed");
        }
        mock.assert_calls(1);
        assert_eq!(std::fs::read(&final_path).unwrap(), body);
    }

    /// The flight registry hands out ONE lock per path (same Arc), and distinct
    /// locks for distinct paths (no false serialization across different files).
    #[test]
    fn file_flight_is_per_path() {
        let a1 = file_flight(Path::new("/tmp/flight-a"));
        let a2 = file_flight(Path::new("/tmp/flight-a"));
        let b = file_flight(Path::new("/tmp/flight-b"));
        assert!(std::sync::Arc::ptr_eq(&a1, &a2), "same path ⇒ same lock");
        assert!(
            !std::sync::Arc::ptr_eq(&a1, &b),
            "different path ⇒ different lock"
        );
    }

    #[test]
    fn permanent_vs_transient_error_classification() {
        // Checksum mismatch + 404 are permanent (fast-fail, no retry).
        assert!(is_permanent_error(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sha mismatch"
        )));
        assert!(is_permanent_error(&std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "http 404"
        )));
        // Timeouts / resets are transient (worth retrying).
        assert!(!is_permanent_error(&std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "read timeout"
        )));
        assert!(!is_permanent_error(&std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        )));
    }

    #[test]
    fn definitive_dns_failure_is_permanent_but_temporary_failure_is_not() {
        let missing = attohttpc::Error::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "host not found",
        ));
        let missing = transport_err(missing);
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        assert!(is_permanent_error(&missing));

        let temporary = attohttpc::Error::from(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "temporary resolver failure",
        ));
        let temporary = transport_err(temporary);
        assert_eq!(temporary.kind(), std::io::ErrorKind::TimedOut);
        assert!(!is_permanent_error(&temporary));

        #[cfg(target_os = "windows")]
        assert!(is_permanent_dns_error(&std::io::Error::from_raw_os_error(
            11001
        )));
        #[cfg(target_os = "linux")]
        assert!(is_permanent_dns_error(&std::io::Error::from_raw_os_error(
            -2
        )));
        #[cfg(target_os = "macos")]
        assert!(is_permanent_dns_error(&std::io::Error::from_raw_os_error(
            8
        )));
    }

    #[test]
    fn http_status_4xx_is_permanent_5xx_transient() {
        // PURE classification (no socket/fixture needed): 4xx → permanent NotFound,
        // 5xx → transient TimedOut.
        let e404 = classify_http_status(404);
        assert_eq!(e404.kind(), std::io::ErrorKind::NotFound);
        assert!(is_permanent_error(&e404));

        let e503 = classify_http_status(503);
        assert_eq!(e503.kind(), std::io::ErrorKind::TimedOut);
        assert!(!is_permanent_error(&e503));
    }

    /// A checksum mismatch must NOT be retried — driven through `ensure_at` (the actual
    /// retry loop) with `retries=3`, so a bug that removed the `is_permanent_error` fast-fail
    /// in `ensure_at` (making it retry a permanent error like any transient one) would show up
    /// as `mock.assert_calls(1)` failing (3 hits instead of 1). `download_once_rejects_bad_checksum`
    /// above already covers the single-attempt rejection itself; this test is specifically about
    /// the retry loop skipping retries for a permanent error, not duplicating that coverage.
    #[test]
    fn ensure_does_not_retry_permanent_checksum_mismatch() {
        let body = b"this body will never match the pin".to_vec();
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/ggml-bogus.bin");
            then.status(200).body(body.clone());
        });
        let url = server.url("/ggml-bogus.bin");

        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            file_name: "ggml-bogus.bin".to_string(),
            url,
            sha256: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        };
        let final_path = dir.path().join("ggml-bogus.bin");
        // retries=3, but a permanent (checksum) error must fast-fail on the FIRST attempt.
        let err = ensure_at(&final_path, &spec, 3, &|_, _| {}).expect_err("checksum must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        mock.assert_calls(1);
    }

    /// The truncation guard's whole reason to exist: a server that DECLARES a
    /// `Content-Length` larger than the body it actually sends (then closes the
    /// connection) simulates a CDN that truncates mid-stream — the historical "succeeds
    /// on the 2nd/3rd attempt" bug. A naive implementation reaches the sha check on the
    /// short `.part` file, mismatches, and mis-classifies it as a PERMANENT `InvalidData`
    /// (no retry). `download_once` must instead classify this as TRANSIENT (`TimedOut`)
    /// BEFORE the sha check, so `ensure_at`'s retry loop re-fetches it. A regression that
    /// reclassifies truncation as permanent would pass every OTHER test in this suite
    /// (every fixture server's Content-Length matches its body exactly) and silently
    /// reintroduce the original bug — this is the one test that would catch it.
    ///
    /// Kept as a hand-rolled `TcpListener` rather than `httpmock` (unlike its siblings above):
    /// `httpmock` always writes a well-formed response whose `Content-Length` matches the body
    /// it sends, so it has no way to declare a length larger than the bytes actually put on the
    /// wire — exactly the mismatch this test needs to construct.
    #[test]
    fn download_once_classifies_truncated_body_as_transient_not_permanent() {
        let full_body =
            b"the quick brown fox jumps over the lazy dog -- the FULL intended body".to_vec();
        let truncated_len = full_body.len() / 2;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/truncated.bin", addr);

        let full_len = full_body.len();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                // Declare the FULL length in the header but only send HALF the bytes, then
                // drop the connection — a truncated response with no transport-level error,
                // exactly like a CDN that resets mid-transfer.
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {full_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&full_body[..truncated_len]);
                let _ = stream.flush();
                // Dropping `stream` here closes it early; the client's next `read` sees a
                // clean EOF (0) well short of the declared Content-Length.
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("truncated.bin");
        let err = download_once(&url, dir.path(), &final_path, "deadbeef", &|_, _| {})
            .expect_err("a truncated body must be rejected");
        let _ = handle.join();

        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "truncation must classify as TRANSIENT (retryable), not permanent: {err}"
        );
        assert!(
            !is_permanent_error(&err),
            "a regression reclassifying truncation as permanent would silently reintroduce \
             the historical 'succeeds on the 2nd/3rd attempt' bug"
        );
        assert!(
            !final_path.exists(),
            "no final file on a truncated download"
        );
    }

    /// Same truncation-guard coverage for `download_to` — used by the onnxruntime archive
    /// fetch and, after the coreml plain-blob integrity fix, effectively every Core ML
    /// download too. A regression here would silently reintroduce the same bug on that path.
    #[test]
    fn download_to_classifies_truncated_body_as_transient_not_permanent() {
        let full_body = b"onnxruntime archive bytes that will be cut off early".to_vec();
        let truncated_len = full_body.len() / 2;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/truncated-archive.tgz", addr);

        let full_len = full_body.len();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {full_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&full_body[..truncated_len]);
                let _ = stream.flush();
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("truncated-archive.tgz");
        let err =
            download_to(&url, &dest, &|_, _| {}).expect_err("a truncated body must be rejected");
        let _ = handle.join();

        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "truncation must classify as TRANSIENT (retryable), not permanent: {err}"
        );
        assert!(!is_permanent_error(&err));
    }

    /// Hermetic coverage for the orphan sweep: an OLD `.tmp*` file (simulating a prior run
    /// killed mid-download) is removed; a RECENT `.tmp*` file (could be a genuinely in-flight
    /// download) survives; a normal, non-`.tmp` file is never touched regardless of age; and
    /// the sweep recurses into subdirectories (the Core ML repos nest their downloads under
    /// per-file subpaths).
    #[test]
    fn sweep_removes_old_orphaned_temp_files_but_spares_recent_and_non_temp() {
        let dir = tempfile::tempdir().unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);

        let old_tmp = dir.path().join(".tmpDEADBEEF");
        std::fs::write(&old_tmp, b"partial, abandoned by a killed prior run").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&old_tmp)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let recent_tmp = dir.path().join(".tmpFEEDFACE");
        std::fs::write(&recent_tmp, b"partial, possibly still in flight").unwrap();

        let real_file = dir.path().join("model.bin");
        std::fs::write(&real_file, b"a complete, final asset").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&real_file)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let sub = dir.path().join("ANE");
        std::fs::create_dir_all(&sub).unwrap();
        let nested_tmp = sub.join(".tmpNESTED");
        std::fs::write(&nested_tmp, b"partial, nested").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&nested_tmp)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        sweep_orphaned_temp_files(dir.path());

        assert!(!old_tmp.exists(), "an old orphaned temp file must be swept");
        assert!(
            recent_tmp.exists(),
            "a fresh temp file must survive (could be in-flight)"
        );
        assert!(real_file.exists(), "non-temp files are never touched");
        assert!(
            !nested_tmp.exists(),
            "the sweep must recurse into subdirectories"
        );
    }
}
