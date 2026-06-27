//! Download engine — atomic temp + rename, retry with backoff, sha-verify, and
//! the installer prefetch fast-path. Blocking `attohttpc` (no tokio); a
//! socket-level per-read inactivity timeout aborts a stalled CDN.

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
    no_query.rsplit('/').next().unwrap_or(no_query)
}

/// If a prefetch dir is set and holds `url`'s file, return its path (else `None`).
fn prefetch_local(url: &str) -> Option<PathBuf> {
    let guard = PREFETCH_DIR.lock().unwrap();
    let dir = guard.as_ref()?;
    let p = dir.join(url_basename(url));
    p.is_file().then_some(p)
}

/// Copy a prefetched file onto `dest` and report it as a completed transfer (so the
/// caller's progress UI jumps to 100% for an instant local copy). Shared by the two
/// download fns' installer fast-paths.
fn copy_prefetched(local: &Path, dest: &Path, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
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

    if verify_sha256(&final_path, &spec.sha256) {
        return Ok(final_path);
    }

    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    std::fs::create_dir_all(dir)?;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries.max(1) {
        match download_once(&spec.url, dir, &final_path, &spec.sha256, progress) {
            Ok(()) => return Ok(final_path),
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

/// All `attohttpc` transport failures (connect refused, read-timeout, reset, TLS,
/// DNS) are TRANSIENT — surface them as `TimedOut` so the retry loop re-attempts.
fn transport_err(e: attohttpc::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Localhost happy-path: serve a known body over a TcpListener and exercise
    /// the temp+rename+verify path of `ensure`'s inner `download_once` WITHOUT a
    /// real CDN. We call `download_once` directly so we control the dir and avoid
    /// touching the user's real model_dir.
    #[test]
    fn download_once_happy_path_over_localhost() {
        let body = b"hello dontspeak model fixture".to_vec();
        let sha = crate::hash::sha256_hex(&body);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/model.bin", addr);

        // Tiny hand-rolled HTTP/1.1 server: one request, 200 + body.
        let server_body = body.clone();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request headers (read until we have them — best
                // effort; we don't need the request to respond).
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    server_body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&server_body);
                let _ = stream.flush();
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("model.bin");
        download_once(&url, dir.path(), &final_path, &sha, &|_, _| {})
            .expect("download_once should succeed and verify");
        let _ = handle.join();

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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/m.bin", addr);
        let server_body = body.clone();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    server_body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&server_body);
                let _ = stream.flush();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("m.bin");
        let err = download_once(&url, dir.path(), &final_path, "deadbeef", &|_, _| {}).unwrap_err();
        let _ = handle.join();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!final_path.exists(), "no final file on checksum mismatch");
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

    /// A checksum mismatch must NOT be retried — a single attempt yields a
    /// permanent `InvalidData`. The server accepts up to 3 connections so a
    /// (buggy) retry would be observable as >1; we assert exactly 1. Driven via
    /// `download_once` directly against a temp dir (hermetic, no `model_dir()`).
    #[test]
    fn ensure_does_not_retry_permanent_checksum_mismatch() {
        let body = b"this body will never match the pin".to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/ggml-bogus.bin", addr);

        // Count how many connections the server accepts. With fast-fail it must be
        // exactly 1 even though retries=3.
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_srv = hits.clone();
        let server_body = body.clone();
        let handle = std::thread::spawn(move || {
            // Accept up to 3 connections so a (buggy) retry would be observable.
            listener.set_nonblocking(false).expect("blocking listener");
            for _ in 0..3 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        hits_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let mut req = [0u8; 1024];
                        let _ = stream.read(&mut req);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            server_body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&server_body);
                        let _ = stream.flush();
                    }
                    Err(_) => break,
                }
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            file_name: "ggml-bogus.bin".to_string(),
            url,
            sha256: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        };
        let final_path = dir.path().join("ggml-bogus.bin");
        let err = download_once(&spec.url, dir.path(), &final_path, &spec.sha256, &|_, _| {})
            .expect_err("checksum must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(is_permanent_error(&err), "checksum mismatch is permanent");
        // One GET = one connection for the single attempt; dropping the handle
        // closes the spawned acceptor.
        drop(handle);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
