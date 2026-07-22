//! Atomic download: temp + rename, Range resume, sha-verify, installer prefetch.
//! Blocking `attohttpc`; per-read inactivity (no total timeout — large models).
//! CDN GETs re-enable redirects (auth-less); probes keep `max_redirections(0)`.
//! Permanent: `InvalidData` (checksum), `NotFound` (4xx/DNS). Transient: `TimedOut`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::hash::{sha256_hex, verify_sha256};
use crate::model_path;
use crate::spec::ModelSpec;

pub(crate) const DEFAULT_RETRIES: u32 = 3;

// Connect fail-fast; 60s per-read inactivity; no total timeout (vs agent-usage Some(total)).
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Cap endless tiny ranges before outer retry.
const MAX_RANGE_SEGMENTS_PER_ATTEMPT: u32 = 64;

/// Auth-less CDN GET: redirects on, total timeout None. Never attach Authorization.
pub(crate) fn http_get_builder(url: &str) -> attohttpc::RequestBuilder {
    const MAX_CDN_REDIRECTIONS: u32 = 5;
    ds_http::request(
        ds_http::Method::GET,
        url,
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        None,
    )
    .follow_redirects(true)
    .max_redirections(MAX_CDN_REDIRECTIONS)
}

// Installer: local dir keyed by prefetch_key (no network).
static PREFETCH_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Prefetch dir (`None` = off). Keyed by [`prefetch_key`].
pub fn set_prefetch_source(dir: Option<PathBuf>) {
    *PREFETCH_DIR.lock().unwrap() = dir;
}

/// Last path segment (query/fragment stripped).
pub fn url_basename(url: &str) -> &str {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let trimmed = no_query.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Installer staging key: URL-hash prefix + basename. Several assets deliberately share
/// an on-disk basename (`config.json`, `tokenizer.json`, … across per-model subdirs), so
/// staging by bare basename would cross-wire them. `prefetch_items` manifests save under
/// this key and [`set_prefetch_source`] lookups match it — the two must stay identical.
pub fn prefetch_key(url: &str) -> String {
    format!(
        "{}-{}",
        &sha256_hex(url.as_bytes())[..12],
        url_basename(url)
    )
}

fn prefetch_local(url: &str) -> Option<PathBuf> {
    let guard = PREFETCH_DIR.lock().unwrap();
    let dir = guard.as_ref()?;
    let p = dir.join(prefetch_key(url));
    p.is_file().then_some(p)
}

// Per-dest lock so shared files (ORT, kokoro_frontend) attach instead of double-fetch.
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

/// Cache hit if SHA matches; else Range-resume download + atomic rename, reporting
/// `(downloaded, total)` progress.
pub fn ensure_with_progress(
    spec: &ModelSpec,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let final_path = model_path(&spec.file_name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve model_dir() (no data dir)",
        )
    })?;
    ensure_at(&final_path, spec, DEFAULT_RETRIES, progress)?;
    Ok(final_path)
}

/// [`ensure_with_progress`] into an EXPLICIT directory instead of the flat `model_dir()` — the
/// per-model subdirectory assets. Same flight-lock /
/// Range-resume / sha-verify / atomic-rename path; creates `dir`. This is also the
/// httpmock seam: tests point a spec's URL at a mock server and `dir` at a tempdir.
pub fn ensure_in_dir(
    dir: &Path,
    spec: &ModelSpec,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let final_path = dir.join(&spec.file_name);
    ensure_at(&final_path, spec, DEFAULT_RETRIES, progress)?;
    Ok(final_path)
}

/// Explicit dest; serialized by [`file_flight`].
pub(crate) fn ensure_at(
    final_path: &Path,
    spec: &ModelSpec,
    retries: u32,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    if let Some(sweep_root) = orphan_sweep_root(final_path, ds_config::model_dir()) {
        sweep_orphans_once(&sweep_root);
    }

    let flight = file_flight(final_path);
    let _in_flight = flight.lock().unwrap();

    if verify_sha256(final_path, &spec.sha256) {
        if let Ok(partial) = resumable_partial_path(final_path) {
            remove_resume_files(&partial, &resumable_metadata_path(&partial));
        }
        return Ok(());
    }

    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let partial = resumable_partial_path(final_path)?;
    let metadata = resumable_metadata_path(&partial);
    let mut state = load_resume_state(&partial, &metadata, spec);

    if verify_sha256(&partial, &spec.sha256) {
        persist_partial(&partial, final_path)?;
        let _ = std::fs::remove_file(&metadata);
        return Ok(());
    }

    // Bad prefetch: zero temp and fall through to network (once).
    if let Some(local) = prefetch_local(&spec.url) {
        copy_prefetched(&local, &partial, progress)?;
        if verify_sha256(&partial, &spec.sha256) {
            persist_partial(&partial, final_path)?;
            let _ = std::fs::remove_file(&metadata);
            return Ok(());
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(&partial)?
            .set_len(0)?;
        state = DownloadState::default();
        let _ = std::fs::remove_file(&metadata);
    }

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries.max(1) {
        let transfer = download_to_network(&spec.url, &partial, progress, &mut state);
        if state.validator.is_some() {
            persist_resume_state(&metadata, spec, &state)?;
        } else {
            let _ = std::fs::remove_file(&metadata);
        }
        let result = transfer.and_then(|()| {
            if verify_sha256(&partial, &spec.sha256) {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "sha256 mismatch on downloaded file",
                ))
            }
        });
        match result {
            Ok(()) => {
                persist_partial(&partial, final_path)?;
                let _ = std::fs::remove_file(&metadata);
                return Ok(());
            }
            Err(e) => {
                if is_permanent_error(&e) {
                    remove_resume_files(&partial, &metadata);
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("permanent download failure (not retried): {e}"),
                    ));
                }
                last_err = Some(std::io::Error::new(
                    e.kind(),
                    format!("attempt {} of {}: {e}", attempt + 1, retries.max(1)),
                ));
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

fn resumable_partial_path(final_path: &Path) -> std::io::Result<PathBuf> {
    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    let name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("model path has no UTF-8 file name"))?;
    Ok(dir.join(format!(".{name}.part")))
}

fn resumable_metadata_path(partial: &Path) -> PathBuf {
    let mut name = partial.as_os_str().to_os_string();
    name.push(".meta");
    PathBuf::from(name)
}

fn load_resume_state(partial: &Path, metadata: &Path, spec: &ModelSpec) -> DownloadState {
    if !partial.is_file() {
        let _ = std::fs::remove_file(metadata);
        return DownloadState::default();
    }
    let expected_identity = resume_identity(spec);
    let state = std::fs::read_to_string(metadata).ok().and_then(|contents| {
        let mut lines = contents.lines();
        (lines.next() == Some("v1") && lines.next() == Some(expected_identity.as_str()))
            .then(|| lines.next())
            .flatten()
            .filter(|validator| !validator.is_empty())
            .map(|validator| DownloadState {
                validator: Some(validator.to_string()),
            })
    });
    if state.is_none() {
        remove_resume_files(partial, metadata);
    }
    state.unwrap_or_default()
}

fn resume_identity(spec: &ModelSpec) -> String {
    sha256_hex(format!("{}\n{}", spec.url, spec.sha256).as_bytes())
}

fn persist_resume_state(
    metadata: &Path,
    spec: &ModelSpec,
    state: &DownloadState,
) -> std::io::Result<()> {
    let Some(validator) = &state.validator else {
        return Ok(());
    };
    let dir = metadata
        .parent()
        .ok_or_else(|| std::io::Error::other("resume metadata path has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    writeln!(tmp, "v1")?;
    writeln!(tmp, "{}", resume_identity(spec))?;
    writeln!(tmp, "{validator}")?;
    tmp.persist(metadata).map_err(|e| e.error)?;
    Ok(())
}

fn persist_partial(partial: &Path, final_path: &Path) -> std::io::Result<()> {
    tempfile::TempPath::try_from_path(partial.to_path_buf())?
        .persist(final_path)
        .map_err(|e| e.error)
}

fn remove_resume_files(partial: &Path, metadata: &Path) {
    let _ = std::fs::remove_file(partial);
    let _ = std::fs::remove_file(metadata);
}

fn orphan_sweep_root(final_path: &Path, model_root: Option<PathBuf>) -> Option<PathBuf> {
    let parent = final_path.parent()?.to_path_buf();
    Some(
        model_root
            .filter(|root| final_path.starts_with(root))
            .unwrap_or(parent),
    )
}

/// 4xx → permanent `NotFound`; other non-success → transient `TimedOut`.
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

    // Definitive NXDOMAIN only; EAI_AGAIN stays retryable.
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

/// Transient except definitive DNS name-not-found.
fn transport_err(e: attohttpc::Error) -> std::io::Error {
    let kind = match e.kind() {
        attohttpc::ErrorKind::Io(source) if is_permanent_dns_error(source) => {
            std::io::ErrorKind::NotFound
        }
        _ => std::io::ErrorKind::TimedOut,
    };
    std::io::Error::new(kind, e.to_string())
}

enum HttpStream {
    Body {
        reader: Box<attohttpc::ResponseReader>,
        start: u64,
        response_len: u64,
        total: u64,
    },
    RangeUnsatisfiable,
}

/// Per-temp retry state. `If-Range` binds suffix to prefix; checksum is final gate.
#[derive(Default)]
pub(crate) struct DownloadState {
    validator: Option<String>,
}

fn parse_content_length(headers: &attohttpc::header::HeaderMap) -> Option<u64> {
    headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// `Content-Range: bytes START-END/TOTAL` — all concrete + consistent, else reject.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = total.parse().ok()?;
    (start <= end && end < total).then_some((start, end, total))
}

/// Strong ETag, else Last-Modified (`If-Range`).
fn response_validator(headers: &attohttpc::header::HeaderMap) -> Option<String> {
    let strong_etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .filter(|etag| !etag.starts_with("W/"));
    strong_etag
        .or_else(|| headers.get("last-modified").and_then(|v| v.to_str().ok()))
        .map(str::to_owned)
}

/// Full/resume GET. 206 only if Content-Range matches offset; 200 after Range = full restart;
/// 416 → caller restart from zero.
fn http_get_stream(
    url: &str,
    resume_from: u64,
    state: &mut DownloadState,
) -> std::io::Result<HttpStream> {
    let mut request = http_get_builder(url);
    if resume_from > 0 {
        request = request.header("Range", format!("bytes={resume_from}-"));
        if let Some(validator) = &state.validator {
            request = request.header("If-Range", validator);
        }
    }
    let resp = request.send().map_err(transport_err)?;
    let status = resp.status().as_u16();
    if status == 416 && resume_from > 0 {
        return Ok(HttpStream::RangeUnsatisfiable);
    }
    if !resp.is_success() {
        return Err(classify_http_status(status));
    }

    let (_status, headers, reader) = resp.split();
    let content_len = parse_content_length(&headers).unwrap_or(0);
    if status == 206 {
        let value = headers
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 206 response is missing Content-Range",
                )
            })?;
        let (start, end, total) = parse_content_range(value).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP 206 response has invalid Content-Range",
            )
        })?;
        let expected_len = end - start + 1;
        if start != resume_from || (content_len > 0 && content_len != expected_len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "HTTP 206 response does not match requested range: requested {resume_from}, \
                     received {start}-{end}/{total} with length {content_len}"
                ),
            ));
        }
        if let Some(received) = response_validator(&headers) {
            if state
                .validator
                .as_ref()
                .is_some_and(|sent| sent != &received)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 206 response changed the download validator",
                ));
            }
            state.validator.get_or_insert(received);
        }
        return Ok(HttpStream::Body {
            reader: Box::new(reader),
            start,
            response_len: expected_len,
            total,
        });
    }

    state.validator = response_validator(&headers);
    Ok(HttpStream::Body {
        reader: Box::new(reader),
        start: 0,
        response_len: content_len,
        total: content_len,
    })
}

/// `InvalidData` / `NotFound` fail fast; else retry.
pub fn is_permanent_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::NotFound
    )
}

/// One download attempt: GET → temp file → verify → atomic rename. The temp file
/// is cleaned up automatically on any early return.
#[cfg(test)]
fn download_once(
    url: &str,
    dir: &Path,
    final_path: &Path,
    expected_sha: &str,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let tmp = tempfile::NamedTempFile::new_in(dir)?;

    // Prefetch if it verifies; corrupt local → zero temp and network.
    if let Some(local) = prefetch_local(url) {
        copy_prefetched(&local, tmp.path(), progress)?;
        if verify_sha256(tmp.path(), expected_sha) {
            tmp.persist(final_path).map_err(|e| e.error)?;
            return Ok(());
        }
        tmp.as_file().set_len(0)?;
    }

    download_to_network(url, tmp.path(), progress, &mut DownloadState::default())?;

    // Verify before rename; complete-body mismatch → permanent InvalidData.
    if !verify_sha256(tmp.path(), expected_sha) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sha256 mismatch on downloaded file",
        ));
    }

    tmp.persist(final_path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
fn download_to(url: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    if let Some(local) = prefetch_local(url) {
        return copy_prefetched(&local, dest, progress);
    }
    download_to_network(url, dest, progress, &mut DownloadState::default())
}

/// GET into `dest` (no checksum); retains validator across retries. Callers verify before land.
pub(crate) fn download_to_with_state(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    state: &mut DownloadState,
) -> std::io::Result<()> {
    if let Some(local) = prefetch_local(url) {
        return copy_prefetched(&local, dest, progress);
    }
    download_to_network(url, dest, progress, state)
}

fn download_to_network(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    state: &mut DownloadState,
) -> std::io::Result<()> {
    let mut resume_from = dest.metadata().map(|m| m.len()).unwrap_or(0);
    if resume_from > MAX_DOWNLOAD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "partial download exceeds size limit",
        ));
    }

    let mut restarted_after_416 = false;
    let mut range_segments = 0;
    loop {
        // Range needs a retained validator; otherwise full GET (checksum still gates land).
        if resume_from > 0 && state.validator.is_none() {
            std::fs::OpenOptions::new()
                .write(true)
                .open(dest)?
                .set_len(0)?;
            resume_from = 0;
        }

        let (mut reader, start, response_len, total) =
            match http_get_stream(url, resume_from, state)? {
                HttpStream::Body {
                    reader,
                    start,
                    response_len,
                    total,
                } => (reader, start, response_len, total),
                HttpStream::RangeUnsatisfiable if !restarted_after_416 => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(dest)?
                        .set_len(0)?;
                    resume_from = 0;
                    restarted_after_416 = true;
                    state.validator = None;
                    continue;
                }
                HttpStream::RangeUnsatisfiable => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "server rejected a full download range",
                    ));
                }
            };
        if total > MAX_DOWNLOAD_BYTES || start.saturating_add(response_len) > MAX_DOWNLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "download Content-Length exceeds size limit",
            ));
        }
        if start > 0 && dest.metadata().map(|m| m.len()).unwrap_or(0) != start {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "partial download changed while its range request was in flight",
            ));
        }

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true);
        if start == 0 {
            options.truncate(true);
        } else {
            options.append(true);
        }
        let mut f = options.open(dest)?;
        let mut buf = [0u8; 64 * 1024];
        let mut received: u64 = 0;
        let mut downloaded = start;
        let mut next_emit = downloaded;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            received += n as u64;
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
        // Preserve a short partial on transient truncation so the caller's next attempt resumes it.
        if response_len > 0 && received < response_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("truncated download: got {received} of {response_len} response bytes"),
            ));
        }
        if response_len > 0 && received > response_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("oversized response: got {received} of {response_len} declared bytes"),
            ));
        }
        if total > 0 && downloaded < total {
            range_segments += 1;
            if range_segments >= MAX_RANGE_SEGMENTS_PER_ATTEMPT {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "server returned {range_segments} short range segments; \
                         downloaded {downloaded} of {total} bytes"
                    ),
                ));
            }
            resume_from = downloaded;
            continue;
        }
        return Ok(());
    }
}

// Orphan `.tmp*` sweep: tempfile default prefix; Drop skips SIGKILL. Once per process
// from ensure_at (covers nested MLX/CUDA dirs under model_dir).

/// Only age ≥ this is swept (in-flight downloads stay).
const MIN_ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

static ORPHAN_SWEEP_ONCE: std::sync::Once = std::sync::Once::new();

fn sweep_orphans_once(dir: &Path) {
    ORPHAN_SWEEP_ONCE.call_once(|| sweep_orphaned_temp_files(dir));
}

/// Recursive best-effort remove of `.tmp*` older than [`MIN_ORPHAN_AGE`].
pub(crate) fn sweep_orphaned_temp_files(dir: &Path) {
    sweep_orphaned_temp_entries(dir, MIN_ORPHAN_AGE);
}

fn sweep_orphaned_temp_entries(dir: &Path, min_age: std::time::Duration) {
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_tmp = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".tmp"));
        if is_tmp {
            let old_enough = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough {
                if file_type.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else if file_type.is_file() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            continue;
        }
        if file_type.is_dir() {
            sweep_orphaned_temp_entries(&path, min_age);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(request).unwrap()
    }

    fn has_header(request: &str, expected: &str) -> bool {
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case(expected))
    }

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

    /// Two URLs sharing a basename must stage under DISTINCT prefetch keys, and the key
    /// must be stable + carry the basename (human-debuggable staging dirs).
    #[test]
    fn prefetch_key_disambiguates_shared_basenames() {
        let a = prefetch_key("https://example.com/qwen3-tts/resolve/rev1/config.json");
        let b = prefetch_key("https://example.com/omnivoice/resolve/rev2/config.json");
        assert_ne!(a, b, "same basename, different URL ⇒ different key");
        for key in [&a, &b] {
            assert!(
                key.ends_with("-config.json"),
                "key keeps the basename: {key}"
            );
            assert!(
                key.len() == "config.json".len() + 13
                    && key.bytes().take(12).all(|c| c.is_ascii_hexdigit()),
                "12-hex-prefix shape: {key}"
            );
        }
        assert_eq!(
            a,
            prefetch_key("https://example.com/qwen3-tts/resolve/rev1/config.json"),
            "key is a pure function of the URL"
        );
    }

    /// `ensure_at` must consume a file staged under `prefetch_key(url)` WITHOUT any
    /// network fetch — the URL points at a dead loopback port, so a keying regression
    /// (e.g. staging by bare basename again) fails instead of silently downloading.
    #[test]
    fn ensure_consumes_a_prefetched_file_staged_under_the_prefetch_key() {
        let body = b"prefetched model bytes".to_vec();
        let sha = crate::hash::sha256_hex(&body);
        let url = "http://127.0.0.1:9/never-dialed/config.json";

        let staging = tempfile::tempdir().unwrap();
        std::fs::write(staging.path().join(prefetch_key(url)), &body).unwrap();
        set_prefetch_source(Some(staging.path().to_path_buf()));

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("config.json");
        let spec = ModelSpec {
            file_name: "config.json".into(),
            url: url.into(),
            sha256: sha,
        };
        let result = ensure_at(&final_path, &spec, 1, &|_, _| {});
        set_prefetch_source(None);
        result.expect("the staged copy satisfies the download with no network");
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
        let partial = resumable_partial_path(&final_path).unwrap();
        assert!(
            !partial.exists(),
            "permanent failures discard partial bytes"
        );
        assert!(!resumable_metadata_path(&partial).exists());
    }

    #[test]
    fn ensure_resumes_across_separate_calls_with_persisted_validator() {
        let body = b"a model body resumed by the daemon's next scheduled attempt".to_vec();
        let split = body.len() / 2;
        let sha = crate::hash::sha256_hex(&body);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());

        let served = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_request(&mut first);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                first,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixture-v1\"\r\n\
                 Connection: close\r\n\r\n",
                served.len()
            )
            .unwrap();
            first.write_all(&served[..split]).unwrap();
            first.flush().unwrap();
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let request = read_request(&mut second);
            assert!(has_header(&request, &format!("Range: bytes={split}-")));
            assert!(has_header(&request, "If-Range: \"fixture-v1\""));
            let remaining = served.len() - split;
            write!(
                second,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {remaining}\r\n\
                 Content-Range: bytes {split}-{}/{}\r\nETag: \"fixture-v1\"\r\n\
                 Connection: close\r\n\r\n",
                served.len() - 1,
                served.len()
            )
            .unwrap();
            second.write_all(&served[split..]).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("model.bin");
        let spec = ModelSpec {
            file_name: "model.bin".into(),
            url,
            sha256: sha,
        };
        let first = ensure_at(&final_path, &spec, 1, &|_, _| {})
            .expect_err("the interrupted first call remains transient");
        assert!(!is_permanent_error(&first));
        let partial = resumable_partial_path(&final_path).unwrap();
        assert_eq!(std::fs::metadata(&partial).unwrap().len(), split as u64);
        assert!(resumable_metadata_path(&partial).is_file());

        ensure_at(&final_path, &spec, 1, &|_, _| {})
            .expect("the next call resumes, verifies, and lands the file");
        server.join().unwrap();

        assert_eq!(std::fs::read(&final_path).unwrap(), body);
        assert!(!partial.exists());
        assert!(!resumable_metadata_path(&partial).exists());
    }

    #[test]
    fn changed_pin_discards_incompatible_resume_files() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("model.bin");
        let partial = resumable_partial_path(&final_path).unwrap();
        let metadata = resumable_metadata_path(&partial);
        std::fs::write(&partial, b"old pinned bytes").unwrap();
        let old = ModelSpec {
            file_name: "model.bin".into(),
            url: "https://example.invalid/model.bin".into(),
            sha256: crate::hash::sha256_hex(b"old pinned bytes"),
        };
        persist_resume_state(
            &metadata,
            &old,
            &DownloadState {
                validator: Some("\"old-version\"".into()),
            },
        )
        .unwrap();
        let new = ModelSpec {
            sha256: crate::hash::sha256_hex(b"new pinned bytes"),
            ..old
        };

        let state = load_resume_state(&partial, &metadata, &new);

        assert!(state.validator.is_none());
        assert!(!partial.exists());
        assert!(!metadata.exists());
    }

    /// A server may return less than the requested suffix. Keep advancing the range with the
    /// original validator until the full representation can be checksummed and renamed.
    #[test]
    fn ensure_resumes_across_short_range_responses_with_a_validator() {
        let body = b"a model body whose second half is resumed instead of fetched twice".to_vec();
        let split = body.len() / 2;
        let short_len = 7;
        let sha = crate::hash::sha256_hex(&body);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());

        let served_body = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_request(&mut first);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                first,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixture-v1\"\r\n\
                 Connection: close\r\n\r\n",
                served_body.len()
            )
            .unwrap();
            first.write_all(&served_body[..split]).unwrap();
            first.flush().unwrap();
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let request = read_request(&mut second);
            assert!(has_header(&request, &format!("Range: bytes={split}-")));
            assert!(has_header(&request, "If-Range: \"fixture-v1\""));
            write!(
                second,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {short_len}\r\n\
                 Content-Range: BYTES {split}-{}/{}\r\nETag: \"fixture-v1\"\r\n\
                 Connection: close\r\n\r\n",
                split + short_len - 1,
                served_body.len()
            )
            .unwrap();
            second
                .write_all(&served_body[split..split + short_len])
                .unwrap();
            drop(second);

            let third_start = split + short_len;
            let (mut third, _) = listener.accept().unwrap();
            let request = read_request(&mut third);
            assert!(has_header(
                &request,
                &format!("Range: bytes={third_start}-")
            ));
            assert!(has_header(&request, "If-Range: \"fixture-v1\""));
            let remaining = served_body.len() - third_start;
            write!(
                third,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {remaining}\r\n\
                 Content-Range: bytes {third_start}-{}/{}\r\nETag: \"fixture-v1\"\r\n\
                 Connection: close\r\n\r\n",
                served_body.len() - 1,
                served_body.len()
            )
            .unwrap();
            third.write_all(&served_body[third_start..]).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("model.bin");
        let spec = ModelSpec {
            file_name: "model.bin".into(),
            url,
            sha256: sha,
        };
        ensure_at(&final_path, &spec, 2, &|_, _| {}).expect("the retry resumes and verifies");
        server.join().unwrap();

        assert_eq!(std::fs::read(&final_path).unwrap(), body);
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "only the atomically persisted final file remains"
        );
    }

    /// Range support is optional. A `200` response to a Range request is a full body, so the
    /// old partial must be truncated before writing rather than appended and corrupted.
    #[test]
    fn range_ignored_by_server_restarts_the_partial_from_zero() {
        let body = b"the complete replacement body".to_vec();
        let partial = b"old-partial";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/ignored.bin", listener.local_addr().unwrap());

        let served_body = body.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(has_header(
                &request,
                &format!("Range: bytes={}-", partial.len())
            ));
            assert!(has_header(&request, "If-Range: \"fixture\""));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served_body.len()
            )
            .unwrap();
            stream.write_all(&served_body).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("ignored.bin");
        std::fs::write(&dest, partial).unwrap();
        let mut state = DownloadState {
            validator: Some("\"fixture\"".into()),
        };
        download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect("a full response restarts safely");
        server.join().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), body);
    }

    /// `If-Range` turns a representation change into a full `200`, which must replace the
    /// retained prefix and its validator instead of mixing bytes from two versions.
    #[test]
    fn changed_validator_restarts_with_the_new_full_representation() {
        let old_body = b"the old representation that gets interrupted".to_vec();
        let new_body = b"the complete new representation".to_vec();
        let split = old_body.len() / 2;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/changed.bin", listener.local_addr().unwrap());

        let served_old = old_body.clone();
        let served_new = new_body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_request(&mut first);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                first,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"old\"\r\n\
                 Connection: close\r\n\r\n",
                served_old.len()
            )
            .unwrap();
            first.write_all(&served_old[..split]).unwrap();
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let request = read_request(&mut second);
            assert!(has_header(&request, &format!("Range: bytes={split}-")));
            assert!(has_header(&request, "If-Range: \"old\""));
            write!(
                second,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"new\"\r\n\
                 Connection: close\r\n\r\n",
                served_new.len()
            )
            .unwrap();
            second.write_all(&served_new).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("changed.bin");
        let mut state = DownloadState::default();
        let first = download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect_err("the first response is truncated");
        assert_eq!(first.kind(), std::io::ErrorKind::TimedOut);
        download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect("the changed representation restarts in full");
        server.join().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), new_body);
    }

    /// When the first response has no validator, discard its partial before retrying so bytes
    /// from two representations can never be combined.
    #[test]
    fn missing_validator_restarts_without_sending_range() {
        let body = b"a complete unvalidated representation".to_vec();
        let split = body.len() / 2;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/no-validator.bin", listener.local_addr().unwrap());

        let served_body = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_request(&mut first);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                first,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served_body.len()
            )
            .unwrap();
            first.write_all(&served_body[..split]).unwrap();
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let request = read_request(&mut second);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                second,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served_body.len()
            )
            .unwrap();
            second.write_all(&served_body).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("no-validator.bin");
        let mut state = DownloadState::default();
        let first = download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect_err("the first response is truncated");
        assert_eq!(first.kind(), std::io::ErrorKind::TimedOut);
        download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect("the unvalidated partial restarts from zero");
        server.join().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), body);
    }

    /// Never append a `206` body unless its range starts at the exact local file length.
    #[test]
    fn mismatched_content_range_is_rejected_without_touching_the_partial() {
        let partial = b"trusted-prefix";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/bad-range.bin", listener.local_addr().unwrap());
        let wrong_start = partial.len() + 1;

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(has_header(
                &request,
                &format!("Range: bytes={}-", partial.len())
            ));
            assert!(has_header(&request, "If-Range: \"fixture\""));
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\n\
                 Content-Range: bytes {wrong_start}-{}/{}\r\nConnection: close\r\n\r\nxyz",
                wrong_start + 2,
                wrong_start + 3
            )
            .unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bad-range.bin");
        std::fs::write(&dest, partial).unwrap();
        let mut state = DownloadState {
            validator: Some("\"fixture\"".into()),
        };
        let err = download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect_err("the range must be rejected");
        server.join().unwrap();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(dest).unwrap(), partial);
    }

    /// A stale oversized partial can produce `416`; retrying once without Range recovers it.
    #[test]
    fn unsatisfiable_range_restarts_once_from_zero() {
        let body = b"remote body".to_vec();
        let partial = b"a local partial longer than the remote body";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/stale.bin", listener.local_addr().unwrap());

        let served_body = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_request(&mut first);
            assert!(has_header(
                &request,
                &format!("Range: bytes={}-", partial.len())
            ));
            assert!(has_header(&request, "If-Range: \"fixture\""));
            first
                .write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n\r\n")
                .unwrap();

            let (mut second, _) = listener.accept().unwrap();
            let request = read_request(&mut second);
            assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
            write!(
                second,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served_body.len()
            )
            .unwrap();
            second.write_all(&served_body).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("stale.bin");
        std::fs::write(&dest, partial).unwrap();
        let mut state = DownloadState {
            validator: Some("\"fixture\"".into()),
        };
        download_to_with_state(&url, &dest, &|_, _| {}, &mut state)
            .expect("416 falls back to a full request");
        server.join().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), body);
    }

    /// Truncation (Content-Length > body) must be transient, not permanent InvalidData.
    /// Hand-rolled TcpListener: httpmock always matches Content-Length to body.
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
                // Content-Length = full body; only half the bytes, then close (clean EOF).
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {full_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&full_body[..truncated_len]);
                let _ = stream.flush();
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

    /// Same truncation-guard coverage for `download_to`, used by the ONNX Runtime archive
    /// fetch. A regression here would silently reintroduce the same bug on that path.
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

    /// The process-wide once guard must cover every per-model subdirectory.
    #[test]
    fn orphan_sweep_starts_at_the_model_root_for_nested_assets() {
        let root = std::path::PathBuf::from("models");
        let nested = root.join("qwen3-tts").join("model.safetensors");
        assert_eq!(orphan_sweep_root(&nested, Some(root.clone())), Some(root));

        let isolated = std::path::PathBuf::from("fixture").join("model.onnx");
        assert_eq!(
            orphan_sweep_root(&isolated, Some(std::path::PathBuf::from("models"))),
            Some(std::path::PathBuf::from("fixture"))
        );
    }

    /// Old orphaned temp files are removed recursively; recent temps and final files survive.
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

        let sub = dir.path().join("mlx");
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

    #[test]
    fn sweep_removes_orphaned_temp_directories_as_units() {
        let root = tempfile::tempdir().unwrap();
        let staging = tempfile::Builder::new().tempdir_in(root.path()).unwrap();
        let staging_path = staging.keep();
        std::fs::create_dir_all(staging_path.join("payload/espeak-ng-data")).unwrap();
        std::fs::write(
            staging_path.join("payload/espeak-ng-data/phondata"),
            b"staged frontend data",
        )
        .unwrap();

        sweep_orphaned_temp_entries(root.path(), std::time::Duration::ZERO);

        assert!(
            !staging_path.exists(),
            "an abandoned tempfile staging tree must be reclaimed in full"
        );
    }

    /// #108 audit fix: public CDN downloads must follow a small number of redirects
    /// (GitHub releases / HF resolve → object storage). Credential probes keep
    /// `ds_http`'s default max_redirections(0); only this auth-less builder opts in.
    #[test]
    fn http_get_builder_follows_cdn_redirect_chain() {
        let server = httpmock::MockServer::start();
        let target = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/cdn-object");
            then.status(200).body("cdn-payload");
        });
        let start = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/releases/download/x");
            then.status(302)
                .header("Location", server.url("/cdn-object"))
                .body("");
        });

        let body = ds_http::read_utf8_limited(
            http_get_builder(&server.url("/releases/download/x"))
                .send()
                .expect("redirect chain must succeed"),
            64,
        )
        .expect("body");
        assert_eq!(body, "cdn-payload");
        start.assert();
        target.assert();
    }
}
