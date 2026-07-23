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

// Per-dest process-local lock so threads attach instead of double-fetch.
fn file_flight(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
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

fn destination_lock_path(final_path: &Path) -> std::io::Result<PathBuf> {
    let dir = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    let name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("model path has no UTF-8 file name"))?;
    Ok(dir.join(format!(".{name}.lock")))
}

/// Cross-process counterpart to [`file_flight`]. The lock file stays in place because
/// deleting it after unlock could let a third process lock a new inode while a waiter
/// still holds the unlinked old one.
fn lock_destination(final_path: &Path) -> std::io::Result<std::fs::File> {
    let lock_path = destination_lock_path(final_path)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    Ok(lock)
}

/// Run one persistent-destination operation under the shared process-local and
/// cross-process locks. The operation must recheck destination readiness after entry.
/// The flight also holds its sweep root's gate for its whole run, so no orphan sweep can
/// reclaim a temp artifact the operation still owns; resolving that root reads ambient
/// `DONTSPEAK_MODEL_DIR`/HOME via [`sweep_root_of`].
pub(crate) fn with_destination_flight<T>(
    final_path: &Path,
    operation: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let parent = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("model path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    // The sweep root can be `final_path` itself (a directory destination), which
    // `create_dir_all(parent)` does not cover.
    let sweep_root = sweep_root_of(final_path).unwrap_or_else(|| parent.to_path_buf());
    std::fs::create_dir_all(&sweep_root)?;
    #[cfg(debug_assertions)]
    {
        let dest_lock = destination_lock_path(final_path)?;
        debug_assert_ne!(
            dest_lock.as_path(),
            sweep_gate_path(&sweep_root).as_path(),
            "destination lock and sweep gate must not share a path (see #214)"
        );
    }
    let _sweep_flight = enter_sweep_gate(&sweep_root);
    let _sweep_lock = lock_sweep_gate_shared(&sweep_root)?;
    let flight = file_flight(final_path);
    let _in_flight = flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _destination_lock = lock_destination(final_path)?;
    operation(parent)
}

/// Acquisition order for [`with_destination_flights`]: sorted so two processes locking
/// overlapping sets always acquire in the same order and cannot deadlock, deduped because
/// nesting a flight inside itself blocks on its own destination lock forever.
fn ordered_flight_paths(paths: &[PathBuf]) -> Vec<&Path> {
    let mut ordered: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    ordered.sort_unstable();
    ordered.dedup();
    ordered
}

/// [`with_destination_flight`] over several destinations at once (a multi-directory asset,
/// or a set installer covering every directory it writes), in [`ordered_flight_paths`] order.
pub(crate) fn with_destination_flights<T>(
    paths: &[PathBuf],
    operation: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let ordered = ordered_flight_paths(paths);
    fn recurse<T>(
        rest: &[&Path],
        operation: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        match rest.split_first() {
            None => operation(),
            Some((first, tail)) => with_destination_flight(first, |_| recurse(tail, operation)),
        }
    }
    recurse(&ordered, operation)
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

/// Explicit dest; serialized within and across processes.
pub(crate) fn ensure_at(
    final_path: &Path,
    spec: &ModelSpec,
    retries: u32,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    if let Some(sweep_root) = sweep_root_of(final_path) {
        sweep_orphans_once(&ORPHAN_SWEEP_DONE, &sweep_root);
    }
    with_destination_flight(final_path, |_| {
        ensure_at_locked(final_path, spec, retries, progress)
    })
}

fn ensure_at_locked(
    final_path: &Path,
    spec: &ModelSpec,
    retries: u32,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    if verify_sha256(final_path, &spec.sha256) {
        if let Ok(partial) = resumable_partial_path(final_path) {
            remove_resume_files(&partial, &resumable_metadata_path(&partial));
        }
        return Ok(());
    }

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

/// Single resolution of a destination's sweep root, so the flight that registers on the
/// gate and the sweep that claims it can never disagree about which root that is.
/// Public so flight-entering tests — here, in `inventory`, and in the engine's `models`
/// backend — can assert their fixture is not shadowed by an ambient `DONTSPEAK_MODEL_DIR`,
/// which would move the gate into the real cache (#204).
pub fn sweep_root_of(final_path: &Path) -> Option<PathBuf> {
    orphan_sweep_root(final_path, ds_config::model_dir())
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

// Orphan `.tmp*` sweep: tempfile's default prefix; Drop skips SIGKILL. Every destination
// flight registers on its sweep root for its whole run and holds `.orphan-sweep.gate` shared;
// the sweep walks only a root with no live flight in this process and no shared lock from
// another one, so it can never unlink an artifact a live download still needs. Counting rather
// than `RwLock`: flights nest (frontend install -> `ensure_in_dir`) on one thread, and
// `RwLock::read` documents a possible panic when the current thread already holds the lock.
// The gate uses `.gate` (not `.{name}.lock`) so it never collides with a destination lock (#214).

/// Only age ≥ this is swept (in-flight downloads stay).
const MIN_ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

fn sweep_gate(root: &Path) -> std::sync::Arc<std::sync::Mutex<usize>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<usize>>>>> = OnceLock::new();
    GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

/// Registers a live flight on `root` until dropped.
struct SweepGateFlight(std::sync::Arc<std::sync::Mutex<usize>>);

fn enter_sweep_gate(root: &Path) -> SweepGateFlight {
    let gate = sweep_gate(root);
    *gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
    SweepGateFlight(gate)
}

impl Drop for SweepGateFlight {
    fn drop(&mut self) {
        let mut count = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *count = count.saturating_sub(1);
    }
}

fn sweep_gate_path(root: &Path) -> PathBuf {
    // Outside the `.{name}.lock` destination-lock namespace (see #214).
    root.join(".orphan-sweep.gate")
}

fn lock_sweep_gate_shared(root: &Path) -> std::io::Result<std::fs::File> {
    let lock = open_sweep_gate_file(root)?;
    lock.lock_shared()?;
    Ok(lock)
}

/// Always a fresh handle: nothing clones or dups it, which is what keeps a nested flight's
/// second shared lock inside the case `File::lock_shared` documents as compatible.
fn open_sweep_gate_file(root: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sweep_gate_path(root))
}

static ORPHAN_SWEEP_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One completed walk per attempt that observed `done` unset. A skipped attempt (missing root,
/// live flight, lock failure) does not latch, so the next `ensure_at` retries instead of losing
/// the process's only attempt. `done` is a parameter rather than a read of the static so a test
/// can drive the latch on its own flag: the static is latched by whichever `ensure_at` test the
/// parallel `ds-model` binary happens to run first. The flag is global while the gate is
/// per-root: production has exactly one model root, and a second root only appears when
/// `model_dir()` is `None` or a destination sits outside it.
fn sweep_orphans_once(done: &std::sync::atomic::AtomicBool, root: &Path) {
    use std::sync::atomic::Ordering;
    if done.load(Ordering::Relaxed) {
        return;
    }
    if sweep_orphaned_temp_files(root) {
        done.store(true, Ordering::Relaxed);
    }
}

/// Recursive best-effort remove of `.tmp*` older than [`MIN_ORPHAN_AGE`], under an exclusive
/// claim on the sweep gate. Returns whether the walk ran.
pub(crate) fn sweep_orphaned_temp_files(root: &Path) -> bool {
    if !root.is_dir() {
        return false;
    }
    let gate = sweep_gate(root);
    // Poisoned = acquirable but a past holder panicked; only contention skips.
    let in_flight = match gate.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return false,
    };
    if *in_flight > 0 {
        return false;
    }
    let Ok(lock) = open_sweep_gate_file(root) else {
        return false;
    };
    if lock.try_lock().is_err() {
        return false;
    }
    sweep_orphaned_temp_entries(root, MIN_ORPHAN_AGE);
    true
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

    const LOCK_CHILD_TARGET: &str = "DS_MODEL_LOCK_CHILD_TARGET";
    const LOCK_CHILD_SOURCE: &str = "DS_MODEL_LOCK_CHILD_SOURCE";
    const LOCK_CHILD_READY: &str = "DS_MODEL_LOCK_CHILD_READY";
    const LOCK_CHILD_RELEASE: &str = "DS_MODEL_LOCK_CHILD_RELEASE";
    const LOCK_TEST_URL: &str = "https://example.invalid/cross-process-model";
    const LOCK_TEST_BYTES: &[u8] = b"cross-process-model";
    const DIR_LOCK_CHILD_TARGET: &str = "DS_MODEL_DIR_LOCK_CHILD_TARGET";
    const DIR_LOCK_CHILD_READY: &str = "DS_MODEL_DIR_LOCK_CHILD_READY";
    const DIR_LOCK_CHILD_RELEASE: &str = "DS_MODEL_DIR_LOCK_CHILD_RELEASE";
    const SWEEP_CHILD_ROOT: &str = "DS_MODEL_SWEEP_CHILD_ROOT";
    const SWEEP_CHILD_READY: &str = "DS_MODEL_SWEEP_CHILD_READY";
    const SWEEP_CHILD_RELEASE: &str = "DS_MODEL_SWEEP_CHILD_RELEASE";

    /// A `.tmp*` artifact aged past [`MIN_ORPHAN_AGE`], with its handle closed — an open
    /// handle would fail `remove_file` on Windows and pass a sweep test vacuously.
    fn aged_orphan(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"an abandoned partial").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - 2 * MIN_ORPHAN_AGE)
            .unwrap();
        path
    }

    #[test]
    fn destination_lock_child() {
        let Ok(target) = std::env::var(LOCK_CHILD_TARGET) else {
            return;
        };
        let source = PathBuf::from(std::env::var_os(LOCK_CHILD_SOURCE).unwrap());
        let ready = PathBuf::from(std::env::var_os(LOCK_CHILD_READY).unwrap());
        let release = PathBuf::from(std::env::var_os(LOCK_CHILD_RELEASE).unwrap());
        let spec = ModelSpec {
            file_name: "model.bin".into(),
            url: LOCK_TEST_URL.into(),
            sha256: sha256_hex(LOCK_TEST_BYTES),
        };
        set_prefetch_source(Some(source));
        ensure_at(Path::new(&target), &spec, 1, &|_, _| {
            std::fs::write(&ready, b"locked").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent did not release child download"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        })
        .unwrap();
        set_prefetch_source(None);
    }

    #[test]
    fn directory_destination_lock_child() {
        let Ok(target) = std::env::var(DIR_LOCK_CHILD_TARGET) else {
            return;
        };
        let target = PathBuf::from(target);
        let ready = PathBuf::from(std::env::var_os(DIR_LOCK_CHILD_READY).unwrap());
        let release = PathBuf::from(std::env::var_os(DIR_LOCK_CHILD_RELEASE).unwrap());
        with_destination_flight(&target, |_| {
            std::fs::write(&ready, b"locked")?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent did not release directory installer"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::fs::create_dir(&target)?;
            std::fs::write(target.join(".complete"), b"ready")
        })
        .unwrap();
    }

    #[test]
    fn active_temp_sweep_child() {
        let Some(root) = std::env::var_os(SWEEP_CHILD_ROOT) else {
            return;
        };
        let root = PathBuf::from(root);
        let ready = PathBuf::from(std::env::var_os(SWEEP_CHILD_READY).unwrap());
        let release = PathBuf::from(std::env::var_os(SWEEP_CHILD_RELEASE).unwrap());
        with_destination_flight(&root.join("model.bin"), |_| {
            let active = root.join(".tmpACTIVE");
            std::fs::write(&active, b"an archive this process is still verifying")?;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&active)?
                .set_modified(std::time::SystemTime::now() - 2 * MIN_ORPHAN_AGE)?;
            std::fs::write(&ready, b"holding")?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent did not release the sweep child"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        })
        .unwrap();
    }

    /// Issue #195: a replacement daemon must wait for the old daemon's downloader instead
    /// of concurrently writing the same deterministic `.part` and `.part.meta` files.
    #[test]
    fn destination_lock_serializes_separate_ensure_processes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.bin");
        let source = tempfile::tempdir().unwrap();
        std::fs::write(
            source.path().join(prefetch_key(LOCK_TEST_URL)),
            LOCK_TEST_BYTES,
        )
        .unwrap();
        let ready = dir.path().join("child-ready");
        let release = dir.path().join("release-child");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("download::tests::destination_lock_child")
            .arg("--nocapture")
            .env(LOCK_CHILD_TARGET, &target)
            .env(LOCK_CHILD_SOURCE, source.path())
            .env(LOCK_CHILD_READY, &ready)
            .env(LOCK_CHILD_RELEASE, &release)
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not acquire destination lock"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let target_for_waiter = target.clone();
        let spec = ModelSpec {
            file_name: "model.bin".into(),
            url: LOCK_TEST_URL.into(),
            sha256: sha256_hex(LOCK_TEST_BYTES),
        };
        let waiter_progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress = waiter_progress.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ensure_at(&target_for_waiter, &spec, 1, &|_, _| {
                progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .unwrap();
            acquired_tx.send(()).unwrap();
        });
        assert!(
            acquired_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "second process must not enter while the first owns the destination"
        );

        std::fs::write(release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("waiter acquires after child exits");
        waiter.join().unwrap();
        assert!(verify_sha256(&target, &sha256_hex(LOCK_TEST_BYTES)));
        assert_eq!(
            waiter_progress.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the waiter must attach to the finalized file without a second transfer"
        );
        let partial = resumable_partial_path(&target).unwrap();
        assert!(!partial.exists());
        assert!(!resumable_metadata_path(&partial).exists());
    }

    #[test]
    fn destination_flight_serializes_directory_installers_across_processes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("runtime");
        let ready = root.path().join("child-ready");
        let release = root.path().join("release-child");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("download::tests::directory_destination_lock_child")
            .arg("--nocapture")
            .env(DIR_LOCK_CHILD_TARGET, &target)
            .env(DIR_LOCK_CHILD_READY, &ready)
            .env(DIR_LOCK_CHILD_RELEASE, &release)
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not acquire directory destination lock"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let target_for_waiter = target.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            with_destination_flight(&target_for_waiter, |_| {
                assert_eq!(
                    std::fs::read(target_for_waiter.join(".complete")).unwrap(),
                    b"ready",
                    "the waiter must recheck the finalized directory after locking"
                );
                acquired_tx.send(()).unwrap();
                Ok(())
            })
            .unwrap();
        });
        assert!(
            acquired_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "second process must not enter while the first owns the directory destination"
        );

        std::fs::write(release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("waiter acquires after child exits");
        waiter.join().unwrap();
        assert!(root.path().join(".runtime.lock").is_file());
    }

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
        let mut entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                ".model.bin.lock".to_string(),
                ".orphan-sweep.gate".to_string(),
                "model.bin".to_string()
            ],
            "only the final file and the two persistent lock files remain"
        );
        assert!(!resumable_partial_path(&final_path).unwrap().exists());
        assert!(!resumable_metadata_path(&resumable_partial_path(&final_path).unwrap()).exists());
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

        assert!(
            sweep_orphaned_temp_files(dir.path()),
            "an uncontended sweep must run"
        );

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

    /// The process-local half of the gate defers the sweep on its own — no file lock involved.
    /// It is the half that still holds where `flock` is emulated by per-process POSIX locks.
    #[test]
    fn a_process_local_flight_defers_the_sweep() {
        let root = tempfile::tempdir().unwrap();
        let orphan = aged_orphan(root.path(), ".tmpINPROC");
        let flight = enter_sweep_gate(root.path());
        assert!(
            !sweep_orphaned_temp_files(root.path()),
            "a live flight must defer the sweep"
        );
        assert!(orphan.is_file(), "a deferred sweep deletes nothing");
        drop(flight);
        assert!(
            sweep_orphaned_temp_files(root.path()),
            "the walk runs once the flight ends"
        );
        assert!(
            !orphan.exists(),
            "the orphan is reclaimed at the next quiet moment"
        );
    }

    /// #199: `with_destination_flight` must register on the gate for its whole run, and the gate
    /// must be keyed per sweep root so an unrelated root still gets cleaned.
    #[test]
    fn a_flight_defers_the_sweep_of_its_own_root_only() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target = root.path().join("model.bin");
        // Guards this fixture against a `DONTSPEAK_MODEL_DIR` that covers `TMPDIR`; it pins the
        // main thread's resolution only, since the worker re-resolves inside the flight (#204).
        assert_eq!(
            sweep_root_of(&target).as_deref(),
            Some(root.path()),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR"
        );
        let owned = aged_orphan(root.path(), ".tmpOWNED");
        let unrelated = aged_orphan(other.path(), ".tmpUNRELATED");

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            with_destination_flight(&target, |_| {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("main thread releases the flight");
                Ok(())
            })
            .unwrap();
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the worker enters the flight");

        // The guard must stay a temporary: held across the sweep below it would make the
        // assertion pass through `try_lock` contention instead of the in-flight count.
        assert_eq!(
            *sweep_gate(root.path()).lock().unwrap(),
            1,
            "with_destination_flight must register on the process-local gate"
        );
        assert!(!sweep_orphaned_temp_files(root.path()));
        assert!(owned.is_file(), "the sweep must spare a root a flight owns");
        assert!(
            sweep_orphaned_temp_files(other.path()),
            "the gate is keyed per sweep root, not global"
        );
        assert!(!unrelated.exists());

        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(sweep_orphaned_temp_files(root.path()));
        assert!(!owned.exists());
    }

    /// #214: gate path must sit outside the `.{name}.lock` destination-lock namespace.
    #[test]
    fn sweep_gate_is_outside_destination_lock_namespace() {
        let root = tempfile::tempdir().unwrap();
        let final_path = root.path().join("orphan-sweep");
        let dest = destination_lock_path(&final_path).unwrap();
        let gate = sweep_gate_path(root.path());
        assert_eq!(dest, root.path().join(".orphan-sweep.lock"));
        assert_eq!(gate, root.path().join(".orphan-sweep.gate"));
        assert_ne!(
            dest, gate,
            "shared-then-exclusive on one path self-deadlocks (#214)"
        );
        // Asset named like the gate file still gets a distinct destination lock.
        let gate_named = root.path().join("orphan-sweep.gate");
        assert_eq!(
            destination_lock_path(&gate_named).unwrap(),
            root.path().join(".orphan-sweep.gate.lock")
        );
        assert_ne!(destination_lock_path(&gate_named).unwrap(), gate);
    }

    /// #214: a flat asset named `orphan-sweep` used to collide gate + destination locks
    /// (`shared` then `exclusive` on the same path) and hang forever.
    #[test]
    fn flat_asset_named_orphan_sweep_enters_flight_without_deadlock() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("orphan-sweep");
        // Forces parent == sweep_root so the pre-fix collision is actually exercised.
        // Same fixture contract as `a_flight_defers_the_sweep_of_its_own_root_only` (#204).
        assert_eq!(
            sweep_root_of(&target).as_deref(),
            Some(root.path()),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR"
        );

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = with_destination_flight(&target, |_| Ok(()));
            let _ = done_tx.send(result);
        });
        match done_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => panic!("flight failed: {err}"),
            Err(_) => {
                panic!("with_destination_flight deadlocked on flat asset named orphan-sweep (#214)")
            }
        }
        worker.join().unwrap();
    }

    /// Sort: two set installers whose planned directories overlap must take the locks in the
    /// same order or they deadlock against each other. Dedup: `with_destination_flight` is not
    /// reentrant, so a repeated path would have the inner call wait on the outer one's own
    /// destination lock forever.
    #[test]
    fn multi_destination_flights_are_sorted_and_deduped() {
        let paths: Vec<PathBuf> = ["/models/mlx/b", "/models/a", "/models/mlx/b", "/models/a"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            ordered_flight_paths(&paths),
            vec![Path::new("/models/a"), Path::new("/models/mlx/b")]
        );
        assert!(ordered_flight_paths(&[]).is_empty());
    }

    /// The dedup above is load-bearing, not cosmetic: without it this hangs the download thread.
    #[test]
    fn a_repeated_destination_enters_its_flight_once() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("repo-dir");
        assert_eq!(
            sweep_root_of(&target).as_deref(),
            Some(root.path()),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let paths = vec![target.clone(), target];
            let _ = done_tx.send(with_destination_flights(&paths, || Ok(())));
        });
        match done_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => panic!("flights failed: {err}"),
            Err(_) => panic!("with_destination_flights self-deadlocked on a duplicate path"),
        }
        worker.join().unwrap();
    }

    /// #199: another process's orphan sweep must not delete an old-looking temp artifact that a
    /// live download still owns.
    #[test]
    fn sweep_spares_a_temp_artifact_an_active_process_owns() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("child-ready");
        let release = root.path().join("release-child");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("download::tests::active_temp_sweep_child")
            .arg("--nocapture")
            .env(SWEEP_CHILD_ROOT, root.path())
            .env(SWEEP_CHILD_READY, &ready)
            .env(SWEEP_CHILD_RELEASE, &release)
            .env_remove("DONTSPEAK_MODEL_DIR")
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not enter the destination flight"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            !sweep_orphaned_temp_files(root.path()),
            "a sweep must skip a root another process is downloading into"
        );
        assert!(
            root.path().join(".tmpACTIVE").is_file(),
            "the active temp survives another process's sweep"
        );

        std::fs::write(&release, b"go").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(sweep_orphaned_temp_files(root.path()));
        assert!(
            !root.path().join(".tmpACTIVE").exists(),
            "the gate defers cleanup rather than disabling it"
        );
    }

    /// The shared half must not serialize unrelated downloads: two destinations under one sweep
    /// root run their flights concurrently. An exclusive root lock fails this in bounded time.
    #[test]
    fn flights_for_different_destinations_run_concurrently_under_one_root() {
        let root = tempfile::tempdir().unwrap();
        let (a_tx, a_rx) = std::sync::mpsc::channel();
        let (b_tx, b_rx) = std::sync::mpsc::channel();

        let a_target = root.path().join("a.bin");
        let a = std::thread::spawn(move || {
            with_destination_flight(&a_target, |_| {
                a_tx.send(()).unwrap();
                Ok(b_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok())
            })
            .unwrap()
        });
        let b_target = root.path().join("b.bin");
        let b = std::thread::spawn(move || {
            with_destination_flight(&b_target, |_| {
                b_tx.send(()).unwrap();
                Ok(a_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok())
            })
            .unwrap()
        });

        assert!(a.join().unwrap(), "flight A must observe B inside its run");
        assert!(b.join().unwrap(), "flight B must observe A inside its run");
    }

    /// #199: a sweep that did not walk must not burn the process's one attempt. Driven through a
    /// local flag — the production static is latched by whichever `ensure_at` test runs first.
    #[test]
    fn a_skipped_sweep_does_not_latch_the_once_guard() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let done = AtomicBool::new(false);

        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("not-created-yet");
        sweep_orphans_once(&done, &missing);
        assert!(
            !done.load(Ordering::Relaxed),
            "a model root that does not exist yet must not burn the process's only sweep"
        );

        let root = tempfile::tempdir().unwrap();
        let orphan = aged_orphan(root.path(), ".tmpRETRY");
        let flight = enter_sweep_gate(root.path());
        sweep_orphans_once(&done, root.path());
        assert!(
            !done.load(Ordering::Relaxed),
            "a sweep deferred by a live flight must not burn the process's only sweep"
        );
        assert!(orphan.is_file());

        drop(flight);
        sweep_orphans_once(&done, root.path());
        assert!(
            done.load(Ordering::Relaxed),
            "the retry runs the walk and latches"
        );
        assert!(!orphan.exists(), "the retried sweep reclaims the orphan");
    }

    /// The latch is real: one completed walk per process, not one per `ensure_at`.
    #[test]
    fn a_completed_sweep_latches_the_once_guard() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let done = AtomicBool::new(false);
        let root = tempfile::tempdir().unwrap();

        let first = aged_orphan(root.path(), ".tmpFIRST");
        sweep_orphans_once(&done, root.path());
        assert!(done.load(Ordering::Relaxed), "a completed walk latches");
        assert!(!first.exists());

        let second = aged_orphan(root.path(), ".tmpSECOND");
        sweep_orphans_once(&done, root.path());
        assert!(
            second.is_file(),
            "the latch suppresses a second walk in the same process"
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
