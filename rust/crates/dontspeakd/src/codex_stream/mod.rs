//! Codex app-server mid-turn narration (#10; docs/STREAMING-NARRATION.md).
//! Session-keyed only; witness parity; no double-speak (HWM); cleanup here (no SessionEnd).

mod client;
mod proto;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ds_config::{NarrateKind, Paths, VoiceConfig};
use ds_narrate::{BatchPayload, NarrationUtterance, StreamBatch};

use client::WsClient;

// ── The session registry (fed by the IPC hook arms, drained here) ────────────────

/// Hook-registered session ids only. Nudge re-arms negative cache; ages out dead ids.
pub(crate) struct SessionRegistry {
    inner: Mutex<RegInner>,
    cv: Condvar,
}

struct RegInner {
    sessions: HashMap<String, Instant>,
    epoch: u64,
    launch_waiters: usize,
    launch_error_seq: u64,
    launch_error: Option<String>,
    connected_endpoint: Option<String>,
}

impl SessionRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(SessionRegistry {
            inner: Mutex::new(RegInner {
                sessions: HashMap::new(),
                epoch: 0,
                launch_waiters: 0,
                launch_error_seq: 0,
                launch_error: None,
                connected_endpoint: None,
            }),
            cv: Condvar::new(),
        })
    }

    /// A hook mentioned this session (SessionStart greet / prompt-submit mark-active):
    /// remember it, refresh its liveness, and wake the supervisor.
    pub(crate) fn nudge(&self, session: &str) {
        if session.trim().is_empty() {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        g.sessions.insert(session.to_string(), Instant::now());
        g.epoch += 1;
        self.cv.notify_all();
    }

    fn snapshot(&self) -> (Vec<String>, u64) {
        let g = self.inner.lock().unwrap();
        (g.sessions.keys().cloned().collect(), g.epoch)
    }

    /// Block one `dontspeak codex` caller until the observer has initialized against an
    /// app-server. The waiter itself is the supervisor's on-demand start signal; no
    /// preference is persisted and concurrent callers collapse onto the same connection.
    pub(crate) fn ensure_remote(&self, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        let mut g = self.inner.lock().unwrap();
        if let Some(endpoint) = &g.connected_endpoint {
            return Ok(endpoint.clone());
        }
        let seen_error = g.launch_error_seq;
        g.launch_waiters += 1;
        g.epoch += 1;
        self.cv.notify_all();

        let result = loop {
            if let Some(endpoint) = &g.connected_endpoint {
                break Ok(endpoint.clone());
            }
            if g.launch_error_seq != seen_error {
                break Err(g
                    .launch_error
                    .clone()
                    .unwrap_or_else(|| "Codex app-server start failed".to_string()));
            }
            let now = Instant::now();
            if now >= deadline {
                break Err("timed out waiting for the Codex app-server".to_string());
            }
            let (next, _) = self.cv.wait_timeout(g, deadline - now).unwrap();
            g = next;
        };

        g.launch_waiters -= 1;
        g.epoch += 1;
        self.cv.notify_all();
        result
    }

    fn launch_requested(&self) -> bool {
        self.inner.lock().unwrap().launch_waiters > 0
    }

    fn launch_ready(&self, endpoint: String) {
        let mut g = self.inner.lock().unwrap();
        g.connected_endpoint = Some(endpoint);
        g.epoch += 1;
        self.cv.notify_all();
    }

    fn launch_detached(&self) {
        let mut g = self.inner.lock().unwrap();
        g.connected_endpoint = None;
        g.epoch += 1;
        self.cv.notify_all();
    }

    fn launch_failed(&self, message: impl Into<String>) {
        let mut g = self.inner.lock().unwrap();
        g.launch_error = Some(message.into());
        g.launch_error_seq += 1;
        g.epoch += 1;
        self.cv.notify_all();
    }

    /// Park until the epoch moves past `seen` or `timeout` elapses; returns the current epoch.
    fn wait_change(&self, seen: u64, timeout: Duration) -> u64 {
        let g = self.inner.lock().unwrap();
        let (g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |g| g.epoch == seen)
            .unwrap();
        g.epoch
    }

    /// Pace one failed attachment without hiding a new synchronous launch request behind the
    /// background reconnect backoff. Unrelated session nudges leave the wait armed; shutdown is
    /// polled in bounded slices because it has no registry notification of its own.
    fn wait_retry(&self, running: &AtomicBool, timeout: Duration, launch_was_requested: bool) {
        const SHUTDOWN_POLL: Duration = Duration::from_millis(100);

        let deadline = Instant::now() + timeout;
        let mut g = self.inner.lock().unwrap();
        loop {
            let now = Instant::now();
            if !running.load(Ordering::Relaxed)
                || now >= deadline
                || (!launch_was_requested && g.launch_waiters > 0)
            {
                return;
            }
            let wait = (deadline - now).min(SHUTDOWN_POLL);
            let (next, _) = self.cv.wait_timeout(g, wait).unwrap();
            g = next;
        }
    }

    fn remove(&self, session: &str) {
        self.inner.lock().unwrap().sessions.remove(session);
    }

    /// Drop entries not nudged within `ttl` (closed terminals; CC/Qwen ids that will
    /// never match a codex thread). Bounds the registry — there is no SessionEnd for Codex.
    fn prune_older_than(&self, ttl: Duration) {
        let mut g = self.inner.lock().unwrap();
        g.sessions.retain(|_, at| at.elapsed() <= ttl);
    }
}

// ── Delta coalescing (bound the state-file RMW frequency) ────────────────────────

/// Per-(session, item) delta buffer: flush into `deliver_batch` on a newline, on age
/// (~150 ms), or on `item/completed` — so a fast token stream doesn't do a locked
/// read-modify-write per token. The per-item `seq` is the monotone `Delta.index` feed;
/// it survives flushes (the entry lives until the item completes).
struct Coalescer {
    bufs: HashMap<(String, String), ItemBuf>,
}

struct ItemBuf {
    pending: String,
    seq: u64,
    last_batch: Instant,
}

impl Coalescer {
    fn new() -> Self {
        Coalescer {
            bufs: HashMap::new(),
        }
    }

    /// Append a delta; returns a batch to feed NOW if the buffer crossed a newline.
    fn on_delta(
        &mut self,
        session: &str,
        item: &str,
        delta: &str,
        now: Instant,
    ) -> Option<(String, StreamBatch)> {
        let buf = self
            .bufs
            .entry((session.to_string(), item.to_string()))
            .or_insert_with(|| ItemBuf {
                pending: String::new(),
                seq: 0,
                last_batch: now,
            });
        buf.pending.push_str(delta);
        if buf.pending.contains('\n') {
            return Self::flush_one(session, item, buf, now);
        }
        None
    }

    /// The item is complete: drop its buffer and emit the AUTHORITATIVE final text as one
    /// cumulative batch (covers deltas missed before attach; `Accum` lets cumulative win).
    fn on_completed(
        &mut self,
        session: &str,
        item: &str,
        final_text: &str,
    ) -> (String, StreamBatch) {
        self.bufs.remove(&(session.to_string(), item.to_string()));
        (
            session.to_string(),
            StreamBatch {
                key: item.to_string(),
                payload: BatchPayload::Cumulative {
                    text: final_text.to_string(),
                },
                is_final: true,
            },
        )
    }

    /// Flush buffers older than `max_age` (the housekeeping tick), or — with
    /// `only_session` — every buffer of one session regardless of age (turn/completed).
    fn flush_aged(
        &mut self,
        now: Instant,
        max_age: Duration,
        only_session: Option<&str>,
    ) -> Vec<(String, StreamBatch)> {
        let mut out = Vec::new();
        let keys: Vec<(String, String)> = self
            .bufs
            .iter()
            .filter(|((sess, _), buf)| {
                !buf.pending.is_empty()
                    && match only_session {
                        Some(s) => sess == s,
                        None => now.duration_since(buf.last_batch) >= max_age,
                    }
            })
            .map(|(k, _)| k.clone())
            .collect();
        for (sess, item) in keys {
            if let Some(buf) = self.bufs.get_mut(&(sess.clone(), item.clone()))
                && let Some(b) = Self::flush_one(&sess, &item, buf, now)
            {
                out.push(b);
            }
        }
        out
    }

    fn flush_one(
        session: &str,
        item: &str,
        buf: &mut ItemBuf,
        now: Instant,
    ) -> Option<(String, StreamBatch)> {
        if buf.pending.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut buf.pending);
        let seq = buf.seq;
        buf.seq += 1;
        buf.last_batch = now;
        Some((
            session.to_string(),
            StreamBatch {
                key: item.to_string(),
                payload: BatchPayload::Delta {
                    index: Some(seq),
                    text,
                },
                is_final: false,
            },
        ))
    }

    /// Drop every buffer belonging to `session`. Called when the session's thread is
    /// evicted from the daemon's loaded list: without this, a partially-streamed item
    /// (deltas received, no `item/completed`) survives in `bufs` and could produce a
    /// spurious utterance if the same session is re-resumed on a different thread within
    /// the same connection -- the stale buffer would flush against the fresh high-water
    /// mark (reset by `clear_session_state`) as a `new` message.
    fn drop_session(&mut self, session: &str) {
        self.bufs.retain(|(sess, _), _| sess != session);
    }
}

// ── Endpoint + daemon-start decisions (pure, unit-tested) ────────────────────────

/// Default loopback listener used by the on-demand Windows launcher.
#[cfg(windows)]
const DEFAULT_WINDOWS_APP_SERVER: &str = "127.0.0.1:4500";

/// Where to attach.
pub(crate) enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(String),
}

impl Endpoint {
    fn key(&self) -> String {
        match self {
            #[cfg(unix)]
            Endpoint::Unix(path) => format!("unix:{}", path.display()),
            Endpoint::Tcp(host) => format!("tcp:{host}"),
        }
    }

    fn remote_arg(&self) -> String {
        match self {
            #[cfg(unix)]
            Endpoint::Unix(path) => format!("unix://{}", path.display()),
            Endpoint::Tcp(host) => format!("ws://{host}"),
        }
    }
}

/// The default codex control socket: `$CODEX_HOME/app-server-control/app-server-control.sock`
/// (verified against openai/codex `app-server-transport`). `codex_home_env` is passed in —
/// NOT read from `std::env` here — so tests never mutate process-global env.
#[cfg(unix)]
pub(crate) fn control_socket_path(
    codex_home_env: Option<&std::ffi::OsStr>,
    paths: &Paths,
) -> PathBuf {
    let home = codex_home_env
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.codex_dir.clone());
    home.join("app-server-control")
        .join("app-server-control.sock")
}

/// A loopback-only `ws://host:port[/…]` → `host:port` for `TcpStream::connect`.
/// Plaintext WebSockets are never accepted for non-loopback hosts.
pub(crate) fn parse_ws_url(url: &str) -> Option<String> {
    let authority = url
        .trim()
        .strip_prefix("ws://")?
        .split('/')
        .next()
        .filter(|host| !host.is_empty() && !host.contains('@'))?;
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']')?.0
    } else {
        authority.rsplit_once(':')?.0
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    loopback.then(|| authority.to_string())
}

/// Engine-owned Windows listeners are deliberately loopback-only. Non-loopback Codex
/// app-servers require an explicit auth mode/token, which this observation client does not
/// own and must never weaken by starting an unauthenticated listener itself.
#[cfg(any(windows, test))]
fn can_auto_start_tcp(host: &str) -> bool {
    host.parse::<std::net::SocketAddr>()
        .is_ok_and(|addr| addr.ip().is_loopback())
}

/// Resolve the endpoint from config: a non-empty `codex_app_server_url` wins (TCP);
/// otherwise use the Unix control socket or the loopback Windows launch default.
pub(crate) fn resolve_endpoint(
    url_override: &str,
    codex_home_env: Option<&std::ffi::OsStr>,
    paths: &Paths,
) -> Option<Endpoint> {
    if !url_override.trim().is_empty() {
        return parse_ws_url(url_override).map(Endpoint::Tcp);
    }
    #[cfg(unix)]
    {
        Some(Endpoint::Unix(control_socket_path(codex_home_env, paths)))
    }
    #[cfg(not(unix))]
    {
        let _ = (codex_home_env, paths);
        Some(Endpoint::Tcp(DEFAULT_WINDOWS_APP_SERVER.to_string()))
    }
}

/// PURE decision for Unix lazy start — extracted so the shell-out itself is never
/// exercised in tests. A connect failure is required rather than mere path absence so a
/// stale control-socket inode does not permanently suppress recovery.
#[cfg(any(unix, test))]
pub(crate) fn should_start_unix_server(
    start_enabled: bool,
    endpoint_unavailable: bool,
    bin_resolved: bool,
    already_owned: bool,
) -> bool {
    start_enabled && endpoint_unavailable && bin_resolved && !already_owned
}

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnixStartKind {
    ManagedDaemon,
    OwnedServer,
}

/// Codex accepts managed-daemon startup only from the standalone binary installed at
/// `$CODEX_HOME/packages/standalone/current/codex`. Homebrew/npm binaries use an ordinary
/// engine-owned app-server instead. Canonicalization covers `current` and configured-bin
/// symlinks without treating every binary named `codex` as managed.
#[cfg(any(unix, test))]
fn unix_start_kind(bin: &Path, codex_home: &Path) -> UnixStartKind {
    let standalone = codex_home.join("packages/standalone/current/codex");
    let is_standalone = bin == standalone
        || std::fs::canonicalize(bin)
            .ok()
            .zip(std::fs::canonicalize(&standalone).ok())
            .is_some_and(|(bin, standalone)| bin == standalone);
    if is_standalone {
        UnixStartKind::ManagedDaemon
    } else {
        UnixStartKind::OwnedServer
    }
}

/// Resolve the codex binary: an absolute config path is used as-is; a bare name is
/// searched on PATH, then the common install dirs (a GUI-launched app has a minimal
/// PATH), including the standalone managed install under the codex home — the SAME
/// `$CODEX_HOME`-or-`paths.codex_dir` resolution [`control_socket_path`] uses, so the
/// binary lookup and the socket can't disagree about where codex lives. On Windows it
/// additionally resolves npm's nested native payload, never a shell shim.
fn resolve_codex_bin(
    cfg_bin: &str,
    home: &Path,
    codex_home: &Path,
    roaming_app_data: Option<&Path>,
) -> Option<PathBuf> {
    #[cfg(not(windows))]
    let _ = roaming_app_data;
    let p = Path::new(cfg_bin);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            #[cfg(windows)]
            if p.extension().is_none() {
                let candidate = dir.join(format!("{cfg_bin}.exe"));
                if candidate.is_file() {
                    return Some(candidate);
                }
                // npm installs extensionless / cmd / PowerShell shims beside the native
                // payload. `Command` needs the real executable for a hidden GUI launch.
                continue;
            }
            let candidate = dir.join(cfg_bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let fallbacks = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        home.join(".local/bin"),
        codex_home.join("packages/standalone/current"),
    ];
    #[cfg(windows)]
    let fallbacks = {
        let mut fallbacks = fallbacks;
        let target = if cfg!(target_arch = "aarch64") {
            "aarch64-pc-windows-msvc"
        } else {
            "x86_64-pc-windows-msvc"
        };
        let roaming = roaming_app_data
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        fallbacks.push(
            roaming
                .join("npm/node_modules/@openai/codex/node_modules")
                .join(if cfg!(target_arch = "aarch64") {
                    "@openai/codex-win32-arm64"
                } else {
                    "@openai/codex-win32-x64"
                })
                .join("vendor")
                .join(target)
                .join("bin"),
        );
        fallbacks
    };
    for dir in fallbacks {
        #[cfg(windows)]
        if p.extension().is_none() {
            let candidate = dir.join(format!("{cfg_bin}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
            continue;
        }
        let candidate = dir.join(cfg_bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn direct_app_server_command(bin: &Path, listen: &str) -> std::process::Command {
    let mut command = std::process::Command::new(bin);
    command
        .args(["app-server", "--listen", listen])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// Launch `codex app-server daemon start` (idempotent upstream; returns once the control
/// socket answers `initialize`). We never own or kill the DAEMON itself — an external-tool
/// shell-out; Codex is never linked. The short-lived
/// STARTER child, however, is reaped on a background thread: this runs inside a driven
/// retry loop in the long-lived engine, and a dropped-unwaited Child per attempt would
/// accumulate zombies until the per-user process limit (spawn-and-drop's repo precedent,
/// ds-helper's afplay, is one-shot — not a loop). Reaping also surfaces the exit status,
/// so an opted-in start that fails (e.g. an older codex without the daemon subcommand)
/// logs WHY instead of reporting "launched" and silently never producing a socket.
#[cfg(unix)]
fn start_daemon(bin: &Path) {
    let spawned = std::process::Command::new(bin)
        .args(["app-server", "daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            log::info!(
                target: "engine",
                "codex-stream: launched `{} app-server daemon start` (endpoint was unavailable)",
                bin.display()
            );
            std::thread::Builder::new()
                .name("ds-codex-daemon-reap".into())
                .spawn(move || match child.wait() {
                    Ok(status) if !status.success() => log::info!(
                        target: "engine",
                        "codex-stream: `app-server daemon start` exited with {status} — no daemon socket will appear"
                    ),
                    Ok(_) => {}
                    Err(e) => log::info!(target: "engine", "codex-stream: daemon start reap failed: {e}"),
                })
                .ok();
        }
        Err(e) => log::info!(target: "engine", "codex-stream: daemon start failed to spawn: {e}"),
    }
}

// ── Crash-orphan sweep ────────────────────────────────────────────────────────────

/// Age past which an untouched `narrate-display-*` file is a crash leftover: its mtime
/// refreshes on every batch, so no live session qualifies.
const ORPHAN_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Remove crash-orphaned per-session narration state (`narrate-display-*.json` and lock/
/// tmp siblings) older than [`ORPHAN_MAX_AGE`]. Codex has no SessionEnd hook and an engine
/// crash skips eviction, so without this sweep the files accumulate forever.
fn sweep_orphaned_state(paths: &Paths) {
    let Some(dir) = paths.narrate_pid.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("narrate-display-") {
            continue;
        }
        if !(name.ends_with(".json") || name.ends_with(".lock") || name.ends_with(".tmp")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > ORPHAN_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ── The attached-connection loop ──────────────────────────────────────────────────

/// Loop pacing — parameterized so tests run in milliseconds; production uses `default()`.
pub(crate) struct Tunables {
    /// Re-read `VoiceConfig` / run TTL scans this often.
    cfg_refresh: Duration,
    /// Retry interval while a registered session hasn't matched a loaded thread yet.
    resolve_retry: Duration,
    /// How many list attempts before an unmatched session is negative-cached (until the
    /// next registry nudge re-arms it).
    resolve_tries: u32,
    /// Periodic full re-list (eviction scan: a resumed thread gone from the loaded list).
    relist: Duration,
    /// Evict a resumed session after this long without any notification/nudge.
    idle_ttl: Duration,
    /// Flush a quiet delta buffer after this long.
    flush_age: Duration,
    /// Bound every attached JSON-RPC request. A peer can keep the WebSocket alive while
    /// dropping one response; without this, list/resume state stays wedged forever.
    request_timeout: Duration,
}

impl Default for Tunables {
    fn default() -> Self {
        Tunables {
            cfg_refresh: Duration::from_secs(2),
            resolve_retry: Duration::from_secs(2),
            resolve_tries: 3,
            relist: Duration::from_secs(60),
            idle_ttl: Duration::from_secs(12 * 3600),
            flush_age: Duration::from_millis(150),
            request_timeout: Duration::from_secs(10),
        }
    }
}

/// Why [`run_attached`] returned without a transport error.
#[derive(Debug, PartialEq)]
enum Detach {
    /// The engine is stopping.
    Shutdown,
    /// `codex_stream` was turned off (or `~/.codex` vanished) — the outer loop parks.
    Disabled,
    /// The configured endpoint changed while attached — reconnect immediately to the new one.
    Reconfigure,
}

struct ResolveState {
    tries: u32,
    next_try: Instant,
    negative: bool,
}

struct ResumedSession {
    session: String,
    last_activity: Instant,
}

/// Whether a `thread/resume` request for `session` is still outstanding — derived from
/// `pending` rather than tracked in a separate set, so the `request_timeout` expiry sweep
/// (which removes stale entries from `pending`) and this check can never disagree.
fn resume_in_flight(session: &str, pending: &HashMap<i64, Pending>) -> bool {
    pending
        .values()
        .any(|p| matches!(p, Pending::Resume { session: s, .. } if s == session))
}

fn resolution_due(
    session: &str,
    state: &ResolveState,
    pending: &HashMap<i64, Pending>,
    now: Instant,
) -> bool {
    !state.negative && state.next_try <= now && !resume_in_flight(session, pending)
}

fn resume_wanted(
    session: &str,
    thread_id: &str,
    resolve: &HashMap<String, ResolveState>,
    resumed: &HashMap<String, ResumedSession>,
    pending: &HashMap<i64, Pending>,
) -> bool {
    resolve.contains_key(session)
        && !resumed.contains_key(thread_id)
        && !resume_in_flight(session, pending)
}

enum Pending {
    LoadedList {
        sent_at: Instant,
    },
    Resume {
        thread_id: String,
        session: String,
        sent_at: Instant,
    },
}

/// A `codex app-server --listen ...` process started and owned by this engine. Windows
/// uses a kill-on-close Job Object; Unix uses the shared process-group lifecycle and is
/// selected only when the resolved binary is not the managed standalone install.
#[cfg(windows)]
struct OwnedAppServer {
    child: std::process::Child,
    endpoint_key: String,
    job: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for OwnedAppServer {
    fn drop(&mut self) {
        // SAFETY: `job` is the live handle returned by CreateJobObjectW and is owned solely
        // by this value, so this is its exactly-once close. Closing the kill-on-close job
        // terminates the whole Codex process tree. Unlike
        // Child::kill in this destructor, the kernel also closes this handle when the host
        // crashes or is force-terminated.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.job);
        }
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
struct OwnedAppServer {
    child: std::process::Child,
    endpoint_key: String,
}

#[cfg(unix)]
impl Drop for OwnedAppServer {
    fn drop(&mut self) {
        const GRACE: Duration = Duration::from_secs(2);
        const POLL: Duration = Duration::from_millis(25);

        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let pgid = self.child.id() as i32;
        ds_proc::kill_group(pgid);
        let deadline = Instant::now() + GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                Ok(None) | Err(_) => break,
            }
        }
        ds_proc::force_kill_group(pgid);
        let _ = self.child.wait();
    }
}

#[cfg(any(unix, windows))]
impl OwnedAppServer {
    fn endpoint_key(&self) -> &str {
        &self.endpoint_key
    }
}

#[cfg(windows)]
fn assign_kill_on_close_job(
    child: &mut std::process::Child,
) -> Result<windows::Win32::Foundation::HANDLE, String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    // SAFETY: every handle comes from the corresponding Win32 constructor and is closed
    // exactly once on every error/success path. `info` is the exact structure required by
    // JobObjectExtendedLimitInformation and its byte length is passed verbatim. The process
    // id belongs to the live child we just spawned.
    unsafe {
        let job = CreateJobObjectW(None, windows::core::PCWSTR::null())
            .map_err(|e| format!("create kill-on-close job: {e}"))?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of_val(&info) as u32,
        ) {
            let _ = CloseHandle(job);
            return Err(format!("configure kill-on-close job: {e}"));
        }
        let process = match OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, child.id()) {
            Ok(process) => process,
            Err(e) => {
                let _ = CloseHandle(job);
                return Err(format!("open Codex child for job assignment: {e}"));
            }
        };
        let assigned = AssignProcessToJobObject(job, process);
        let _ = CloseHandle(process);
        if let Err(e) = assigned {
            let _ = CloseHandle(job);
            return Err(format!("assign Codex child to kill-on-close job: {e}"));
        }
        Ok(job)
    }
}

#[cfg(windows)]
fn start_tcp_app_server(bin: &Path, host: &str) -> Result<OwnedAppServer, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = direct_app_server_command(bin, &format!("ws://{host}"));
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|e| format!("start `{}` on ws://{host}: {e}", bin.display()))?;
    match assign_kill_on_close_job(&mut child) {
        Ok(job) => Ok(OwnedAppServer {
            child,
            endpoint_key: format!("tcp:{host}"),
            job,
        }),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(e)
        }
    }
}

#[cfg(unix)]
fn start_unix_app_server(bin: &Path, socket: &Path) -> Result<OwnedAppServer, String> {
    let listen = format!("unix://{}", socket.display());
    let mut command = direct_app_server_command(bin, &listen);
    ds_proc::set_new_process_group(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("start `{}` on {listen}: {e}", bin.display()))?;
    Ok(OwnedAppServer {
        child,
        endpoint_key: format!("unix:{}", socket.display()),
    })
}

impl Pending {
    fn sent_at(&self) -> Instant {
        match self {
            Pending::LoadedList { sent_at } | Pending::Resume { sent_at, .. } => *sent_at,
        }
    }
}

/// Drive one attached connection to completion: resolve registered sessions to loaded
/// threads, resume + seed witnesses, translate deltas/completions into `StreamBatch`es
/// through the shared file-backed core, and forward utterances via `speak`. Returns
/// `Ok(detach)` on an orderly stand-down, `Err` on disconnect (the caller reconnects with
/// backoff; the on-disk high-water mark makes the re-attach dedup-safe).
#[allow(clippy::too_many_arguments)]
fn run_attached<S: Read + Write>(
    ws: &mut WsClient<S>,
    paths: &Paths,
    running: &AtomicBool,
    registry: &SessionRegistry,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
    tun: &Tunables,
    connected_endpoint: Option<&str>,
) -> Result<Detach, String> {
    let mut pending: HashMap<i64, Pending> = HashMap::new();
    let mut resumed: HashMap<String, ResumedSession> = HashMap::new(); // thread → session
    let mut resolve: HashMap<String, ResolveState> = HashMap::new(); // session → progress
    let mut coalescer = Coalescer::new();

    let mut cfg = VoiceConfig::load(paths);
    let mut last_cfg = Instant::now();
    // Force an immediate first list (an Instant can't reliably be rewound, so model
    // "never listed" explicitly).
    let mut last_list: Option<Instant> = None;
    let (initial_sessions, mut epoch) = registry.snapshot();
    for s in initial_sessions {
        resolve.insert(
            s,
            ResolveState {
                tries: 0,
                next_try: Instant::now(),
                negative: false,
            },
        );
    }
    let mut list_in_flight = false;

    loop {
        if !running.load(Ordering::Relaxed) {
            return Ok(Detach::Shutdown);
        }
        let now = Instant::now();
        // A read timeout only proves that no frame arrived during this tick; the socket can
        // remain healthy while one JSON-RPC response is lost. Expire individual requests so
        // list/resume resolution retries instead of remaining permanently in flight.
        let expired: Vec<i64> = pending
            .iter()
            .filter(|(_, request)| now.duration_since(request.sent_at()) >= tun.request_timeout)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            match pending.remove(&id) {
                Some(Pending::LoadedList { .. }) => {
                    list_in_flight = false;
                    log::debug!(
                        target: "codex-stream",
                        "thread/loaded/list request timed out — retrying"
                    );
                }
                Some(Pending::Resume { session, .. }) => {
                    if let Some(state) = resolve.get_mut(&session) {
                        state.next_try = now + tun.resolve_retry;
                    }
                    log::debug!(
                        target: "codex-stream",
                        "thread/resume request timed out for session {session} — retrying"
                    );
                }
                None => {}
            }
        }

        // Config gate + TTL scans, throttled.
        if now.duration_since(last_cfg) >= tun.cfg_refresh {
            last_cfg = now;
            cfg = VoiceConfig::load(paths);
            if !cfg.codex_stream || !paths.codex_dir.exists() {
                // Orderly stand-down: detach from every thread so the server can unload.
                for thread_id in resumed.keys() {
                    let id = ws.next_id();
                    let _ = ws.send(proto::thread_unsubscribe_request(id, thread_id));
                }
                return Ok(Detach::Disabled);
            }
            if let Some(connected) = connected_endpoint {
                let configured = resolve_endpoint(
                    &cfg.codex_app_server_url,
                    std::env::var_os("CODEX_HOME").as_deref(),
                    paths,
                )
                .map(|endpoint| endpoint.key());
                if configured.as_deref() != Some(connected) {
                    for thread_id in resumed.keys() {
                        let id = ws.next_id();
                        let _ = ws.send(proto::thread_unsubscribe_request(id, thread_id));
                    }
                    return Ok(Detach::Reconfigure);
                }
            }
            // Idle-TTL eviction: a session silent this long is gone (Codex has no
            // SessionEnd hook — cleanup is owned HERE).
            let dead: Vec<String> = resumed
                .iter()
                .filter(|(_, r)| r.last_activity.elapsed() > tun.idle_ttl)
                .map(|(t, _)| t.clone())
                .collect();
            for thread_id in dead {
                if let Some(r) = resumed.remove(&thread_id) {
                    let id = ws.next_id();
                    let _ = ws.send(proto::thread_unsubscribe_request(id, &thread_id));
                    ds_narrate::clear_session_state(paths, &r.session);
                    registry.remove(&r.session);
                    resolve.remove(&r.session);
                    log::info!(
                        target: "engine",
                        "codex-stream: evicted idle session {} (ttl) client=codex",
                        r.session
                    );
                }
            }
            registry.prune_older_than(tun.idle_ttl);
        }

        // Registry nudges: new/refreshed sessions re-arm resolution (negatives included —
        // "negative-cached until the next nudge").
        let (cur_sessions, cur_epoch) = registry.snapshot();
        if cur_epoch != epoch {
            epoch = cur_epoch;
            for s in cur_sessions {
                let known = resumed.values().any(|r| r.session == s);
                if !known {
                    let entry = resolve.entry(s).or_insert(ResolveState {
                        tries: 0,
                        next_try: now,
                        negative: false,
                    });
                    if entry.negative {
                        entry.negative = false;
                        entry.tries = 0;
                        entry.next_try = now;
                    }
                }
            }
        }

        // Need a fresh loaded-thread list? (unresolved sessions due a retry, or the
        // periodic eviction scan.)
        let unresolved_due = resolve
            .iter()
            .any(|(session, st)| resolution_due(session, st, &pending, now));
        let relist_due = last_list.is_none_or(|at| now.duration_since(at) >= tun.relist);
        if !list_in_flight && (unresolved_due || relist_due) {
            let id = ws.next_id();
            ws.send(proto::thread_loaded_list_request(id))?;
            pending.insert(id, Pending::LoadedList { sent_at: now });
            list_in_flight = true;
            last_list = Some(now);
        }

        // One read tick (the socket read timeout paces this loop).
        match ws.read_text()? {
            None => {}
            Some(text) => match proto::parse_incoming(&text) {
                proto::Incoming::Response { id, result } => match pending.remove(&id) {
                    Some(Pending::LoadedList { .. }) => {
                        list_in_flight = false;
                        // A JSON-RPC ERROR reply (`result: None`) must NOT drive eviction:
                        // conflating it with "zero threads loaded" would wipe every attached
                        // session's witness + spoken-offset state on one transient server
                        // error, and the next batch (or Stop) would re-speak everything —
                        // breaking this module's never-double-speak contract. Keep all
                        // sessions; the next relist tick retries.
                        if let Some(result) = result.as_ref() {
                            let loaded = proto::loaded_thread_ids(result);
                            // Evict resumed threads that disappeared from the daemon.
                            let gone: Vec<String> = resumed
                                .keys()
                                .filter(|t| !loaded.contains(t))
                                .cloned()
                                .collect();
                            for thread_id in gone {
                                if let Some(r) = resumed.remove(&thread_id) {
                                    ds_narrate::clear_session_state(paths, &r.session);
                                    registry.remove(&r.session);
                                    resolve.remove(&r.session);
                                    coalescer.drop_session(&r.session);
                                    log::info!(
                                        target: "engine",
                                        "codex-stream: session {} unloaded from the app-server — evicted client=codex",
                                        r.session
                                    );
                                }
                            }
                            // Resume every loaded thread that maps to a registered session.
                            for thread_id in &loaded {
                                let session = proto::session_for_thread(thread_id);
                                let wanted = resume_wanted(
                                    &session, thread_id, &resolve, &resumed, &pending,
                                );
                                if wanted {
                                    let id = ws.next_id();
                                    ws.send(proto::thread_resume_request(id, thread_id))?;
                                    pending.insert(
                                        id,
                                        Pending::Resume {
                                            thread_id: thread_id.clone(),
                                            session,
                                            sent_at: now,
                                        },
                                    );
                                }
                            }
                            // Sessions still unmatched: schedule the retry / negative-cache.
                            // Skip a session with a resume genuinely in flight — its thread
                            // just wasn't in THIS listing snapshot, which races the resume
                            // rather than proving the thread is actually gone.
                            for (session, st) in resolve.iter_mut() {
                                let matched = loaded
                                    .iter()
                                    .any(|t| &proto::session_for_thread(t) == session);
                                if !matched && !st.negative && !resume_in_flight(session, &pending)
                                {
                                    st.tries += 1;
                                    if st.tries >= tun.resolve_tries {
                                        st.negative = true; // until the next registry nudge
                                    } else {
                                        st.next_try = now + tun.resolve_retry;
                                    }
                                }
                            }
                        } else {
                            log::debug!(
                                target: "codex-stream",
                                "thread/loaded/list returned an error — keeping sessions; will relist"
                            );
                        }
                    }
                    Some(Pending::Resume {
                        thread_id, session, ..
                    }) => match result {
                        Some(result) => {
                            // Correlation cross-check: the resume response's sessionId is
                            // authoritative per the codex docs; log divergence (forked
                            // threads) rather than mis-scope the witness.
                            if let Some(sid) = proto::resumed_session_id(&result)
                                && sid != session
                            {
                                log::info!(
                                    target: "engine",
                                    "codex-stream: thread {thread_id} reports sessionId {sid} != {session} — narrating under the hook session id client=codex"
                                );
                            }
                            // The witness, IMMEDIATELY on resume — closes the short-turn
                            // race where Stop could fire before the first coalesced flush.
                            ds_narrate::seed_witness(paths, &session);
                            resolve.remove(&session);
                            log::info!(
                                target: "engine",
                                "codex-stream: attached to session {session} (thread {thread_id}) client=codex"
                            );
                            resumed.insert(
                                thread_id,
                                ResumedSession {
                                    session,
                                    last_activity: Instant::now(),
                                },
                            );
                        }
                        None => {
                            // Resume refused — retry via the normal resolve cycle.
                            if let Some(st) = resolve.get_mut(&session) {
                                st.next_try = now + tun.resolve_retry;
                            }
                        }
                    },
                    None => {}
                },
                proto::Incoming::AgentMessageDelta {
                    thread_id,
                    item_id,
                    delta,
                } => {
                    if let Some(r) = resumed.get_mut(&thread_id) {
                        r.last_activity = Instant::now();
                        let session = r.session.clone();
                        if let Some((sess, batch)) =
                            coalescer.on_delta(&session, &item_id, &delta, now)
                        {
                            flush(paths, &cfg, mic_active, speak, &sess, &batch);
                        }
                    }
                }
                proto::Incoming::AgentMessageCompleted {
                    thread_id,
                    item_id,
                    text,
                } => {
                    if let Some(r) = resumed.get_mut(&thread_id) {
                        r.last_activity = Instant::now();
                        let session = r.session.clone();
                        let (sess, batch) = coalescer.on_completed(&session, &item_id, &text);
                        flush(paths, &cfg, mic_active, speak, &sess, &batch);
                    }
                }
                proto::Incoming::TurnCompleted { thread_id } => {
                    if let Some(r) = resumed.get_mut(&thread_id) {
                        r.last_activity = Instant::now();
                        let session = r.session.clone();
                        for (sess, batch) in
                            coalescer.flush_aged(now, Duration::ZERO, Some(&session))
                        {
                            flush(paths, &cfg, mic_active, speak, &sess, &batch);
                        }
                    }
                }
                proto::Incoming::Other => {}
            },
        }

        // Age-flush quiet buffers (the ~150 ms cadence bound).
        for (sess, batch) in coalescer.flush_aged(Instant::now(), tun.flush_age, None) {
            flush(paths, &cfg, mic_active, speak, &sess, &batch);
        }
        // A completed item may have no later app-server event to trigger a replay. Retry
        // persisted admission failures on the housekeeping cadence so queue drainage alone
        // is enough to unblock them.
        for session in resumed.values().map(|resumed| resumed.session.as_str()) {
            if let Err(error) =
                ds_narrate::retry_pending(paths, session, |utterance| speak(session, utterance))
            {
                log::debug!(target: "codex_stream", "pending narration still blocked: {error}");
            }
        }
    }
}

/// One batch through the shared core: the narrate gate, the file-backed step, the
/// per-utterance forward. `cfg` is the supervisor's (periodically refreshed) config read.
fn flush(
    paths: &Paths,
    cfg: &VoiceConfig,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
    session: &str,
    batch: &StreamBatch,
) {
    let digests_on = cfg.narrates(NarrateKind::Digests);
    let shorts_on = cfg.narrates(NarrateKind::Shorts);
    if !digests_on && !shorts_on {
        return;
    }
    if let Err(error) = ds_narrate::deliver_batch(
        paths,
        session,
        batch,
        mic_active(),
        digests_on,
        shorts_on,
        |utterance| speak(session, utterance),
    ) {
        log::warn!(target: "codex_stream", "narration rejected: {error}");
    }
}

// ── The supervisor thread ─────────────────────────────────────────────────────────

/// Reconnect backoff bounds (1 s → 30 s, doubling).
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CEIL: Duration = Duration::from_secs(30);
/// A launcher is synchronously waiting for the observer endpoint, so retry quickly while the
/// newly started app-server binds. The normal exponential backoff resumes without a waiter.
const LAUNCH_RETRY_DELAY: Duration = Duration::from_millis(100);
/// An attachment must have held this long for its loss to earn the prompt floor retry;
/// anything shorter is treated like an attach failure (doubling backoff). Gates the
/// hot-loop where a peer keeps accepting handshake+initialize and then dropping us
/// (version-skew server, misbehaving ws:// proxy) — see [`next_backoff`].
const STABLE_ATTACH: Duration = Duration::from_secs(60);

/// PURE backoff step for the reconnect loop (unit-tested): a STABLE attachment's loss
/// resets to the floor; an unstable one (or a plain attach failure) doubles toward the
/// ceiling. Background retries sleep the returned delay; a synchronous launcher uses
/// [`retry_delay`] to poll the endpoint promptly without creating a zero-sleep hot loop.
fn next_backoff(stable_attachment: bool, backoff: Duration) -> Duration {
    if stable_attachment {
        BACKOFF_FLOOR
    } else {
        (backoff * 2).min(BACKOFF_CEIL)
    }
}

fn retry_delay(backoff: Duration, launch_requested: bool) -> Duration {
    if launch_requested {
        backoff.min(LAUNCH_RETRY_DELAY)
    } else {
        backoff
    }
}

fn should_park_supervisor(
    stream_enabled: bool,
    codex_present: bool,
    have_sessions: bool,
    auto_start: bool,
) -> bool {
    !stream_enabled || !codex_present || (!have_sessions && !auto_start)
}

/// Never launch an app-server more often than this.
const SERVER_START_MIN_GAP: Duration = Duration::from_secs(60);

/// Spawn the supervisor beside the engine's other background threads. Self-gating: it
/// parks (cheap condvar wait) while `codex_stream` is off, `~/.codex` is absent, or no
/// session has been registered — so on a codex-less machine it costs one parked thread.
pub(crate) fn spawn_supervisor(
    paths: Paths,
    running: Arc<AtomicBool>,
    registry: Arc<SessionRegistry>,
    mic: ds_platform::MicState,
    ttsq: Arc<crate::ttsq::TtsQueue>,
) {
    std::thread::Builder::new()
        .name("ds-codex-stream".into())
        .spawn(move || {
            sweep_orphaned_state(&paths);
            let mic_active = move || mic.is_active();
            // Utterances ride the SAME per-session queue path as hook narration —
            // per-session hold/active routing and scoped barge apply because the session
            // id matches the one the hooks use (voice follows the Codex source).
            let mut speak = move |session: &str, utterance: &NarrationUtterance| {
                ttsq.enqueue_narration(
                    utterance.text.clone(),
                    ds_config::ClientSource::Codex,
                    Some(session.to_string()),
                    Some(utterance.id.clone()),
                    Some(utterance.detection_text.clone()).filter(|s| !s.is_empty()),
                    Some(utterance.message_key.clone()),
                )
            };
            supervise(
                &paths,
                &running,
                &registry,
                &mic_active,
                &mut speak,
                &Tunables::default(),
            );
        })
        .ok();
}

/// The outer connect loop: park while gated, resolve the endpoint, attach (optionally
/// starting the managed or engine-owned lifecycle), then hand the connection to
/// [`run_attached`]; reconnect with capped backoff on disconnect.
fn supervise(
    paths: &Paths,
    running: &AtomicBool,
    registry: &SessionRegistry,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
    tun: &Tunables,
) {
    let mut backoff = BACKOFF_FLOOR;
    let mut epoch = 0u64;
    let mut last_server_start: Option<Instant> = None;
    // Once `dontspeak codex` has requested the remote path, keep it warm for this engine
    // lifetime. Later launches then reuse the same observer/server instead of racing a
    // teardown in the gap between the endpoint reply and Codex's SessionStart hook.
    let mut launch_kept_warm = false;
    // One WARN per unresolvable-binary streak: this branch re-runs every backoff pass,
    // and an explicit opt-in that silently no-ops (nvm/npm-prefix installs off the GUI
    // PATH) is the failure the audit flagged — but a per-pass WARN would spam the log.
    let mut warned_bin_unresolvable = false;
    #[cfg(any(unix, windows))]
    let mut owned_server: Option<OwnedAppServer> = None;
    while running.load(Ordering::Relaxed) {
        #[cfg(any(unix, windows))]
        if let Some(server) = owned_server.as_mut() {
            match server.child.try_wait() {
                Ok(Some(status)) => {
                    log::info!(
                        target: "engine",
                        "codex-stream: engine-owned app-server exited with {status} client=codex"
                    );
                    if registry.launch_requested() {
                        registry.launch_failed(format!(
                            "engine-owned Codex app-server exited with {status} before becoming ready"
                        ));
                    }
                    owned_server = None;
                }
                Ok(None) => {}
                Err(e) => {
                    log::info!(
                        target: "engine",
                        "codex-stream: could not probe engine-owned app-server: {e} client=codex"
                    );
                    if registry.launch_requested() {
                        registry.launch_failed(format!(
                            "could not monitor engine-owned Codex app-server: {e}"
                        ));
                    }
                    owned_server = None;
                }
            }
        }
        let cfg = VoiceConfig::load(paths);
        let (sessions, cur_epoch) = registry.snapshot();
        let launch_requested = registry.launch_requested();
        if launch_requested && !cfg.codex_stream {
            registry.launch_failed(
                "Codex streaming is disabled; enable `codex_stream` in DontSpeak config",
            );
            epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_millis(100));
            continue;
        }
        if launch_requested && !paths.codex_dir.exists() {
            registry.launch_failed("Codex is not installed or its config directory is missing");
            epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_millis(100));
            continue;
        }
        #[cfg(any(unix, windows))]
        if !cfg.codex_daemon && !launch_kept_warm && !launch_requested && owned_server.is_some() {
            owned_server = None;
            log::info!(
                target: "engine",
                "codex-stream: stopped the engine-owned app-server after auto-start was disabled client=codex"
            );
        }
        let force_start = cfg.codex_daemon || launch_kept_warm || launch_requested;
        // Auto-start must not wait for a registered session: a remote TUI cannot connect
        // (and therefore cannot fire SessionStart) until the app-server already exists.
        // Without auto-start, preserve the cheap no-session park.
        if should_park_supervisor(
            cfg.codex_stream,
            paths.codex_dir.exists(),
            !sessions.is_empty(),
            force_start,
        ) {
            #[cfg(any(unix, windows))]
            if owned_server.is_some() {
                owned_server = None;
                log::info!(
                    target: "engine",
                    "codex-stream: stopped the engine-owned app-server client=codex"
                );
            }
            // Parked. A nudge (or the timeout) re-checks the gate.
            epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_secs(5));
            continue;
        }
        let endpoint = resolve_endpoint(
            &cfg.codex_app_server_url,
            std::env::var_os("CODEX_HOME").as_deref(),
            paths,
        );
        let endpoint_key = endpoint.as_ref().map(Endpoint::key);
        #[cfg(any(unix, windows))]
        if let Some(server) = owned_server.as_ref()
            && endpoint_key.as_deref() != Some(server.endpoint_key())
        {
            owned_server = None;
            log::info!(
                target: "engine",
                "codex-stream: stopped the engine-owned app-server after endpoint change client=codex"
            );
        }
        let remote_endpoint = endpoint.as_ref().map(Endpoint::remote_arg);
        let attempt_started = Instant::now();
        let attached: Result<Result<Detach, String>, String> = match endpoint {
            None => {
                if launch_requested {
                    registry.launch_failed(
                        "codex_app_server_url must be an unauthenticated loopback ws:// endpoint",
                    );
                }
                // Invalid or unsupported override: park until config changes.
                epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_secs(30));
                continue;
            }
            #[cfg(unix)]
            Some(Endpoint::Unix(sock)) => match std::os::unix::net::UnixStream::connect(&sock) {
                Err(connect_error) => {
                    let endpoint_unavailable = matches!(
                        connect_error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    );
                    let codex_home = std::env::var_os("CODEX_HOME")
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| paths.codex_dir.clone());
                    let bin = if force_start {
                        resolve_codex_bin(&cfg.codex_bin, &paths.home, &codex_home, None)
                    } else {
                        None
                    };
                    if force_start && endpoint_unavailable && bin.is_none() {
                        if launch_requested {
                            registry.launch_failed(format!(
                                "Codex executable {:?} was not found; set codex_bin to its full path",
                                cfg.codex_bin
                            ));
                        }
                        if !warned_bin_unresolvable {
                            warned_bin_unresolvable = true;
                            log::info!(
                                target: "engine",
                                "codex-stream: app-server start is enabled but `{}` was not found on PATH or the known install dirs — set codex_bin to the binary's full path client=codex",
                                cfg.codex_bin
                            );
                        }
                    } else if bin.is_some() {
                        warned_bin_unresolvable = false;
                    }
                    let throttled =
                        last_server_start.is_some_and(|at| at.elapsed() < SERVER_START_MIN_GAP);
                    if !throttled
                        && should_start_unix_server(
                            force_start,
                            endpoint_unavailable,
                            bin.is_some(),
                            owned_server.is_some(),
                        )
                        && let Some(bin) = bin
                    {
                        last_server_start = Some(Instant::now());
                        match unix_start_kind(&bin, &codex_home) {
                            UnixStartKind::ManagedDaemon => start_daemon(&bin),
                            UnixStartKind::OwnedServer => {
                                match start_unix_app_server(&bin, &sock) {
                                    Ok(server) => {
                                        owned_server = Some(server);
                                        log::info!(
                                            target: "engine",
                                            "codex-stream: started engine-owned app-server on unix://{} client=codex",
                                            sock.display()
                                        );
                                    }
                                    Err(e) => {
                                        if launch_requested {
                                            registry.launch_failed(e.clone());
                                        }
                                        log::info!(target: "engine", "codex-stream: {e} client=codex");
                                    }
                                }
                            }
                        }
                    }
                    Err(format!("connect {}: {connect_error}", sock.display()))
                }
                Ok(stream) => WsClient::handshake(stream, "ws://localhost/")
                    .and_then(|mut ws| {
                        ws.initialize(Duration::from_secs(10))?;
                        Ok(ws)
                    })
                    .map(|mut ws| {
                        registry.launch_ready(
                            remote_endpoint
                                .clone()
                                .expect("an endpoint produced this match arm"),
                        );
                        if launch_requested {
                            launch_kept_warm = true;
                        }
                        let result = run_attached(
                            &mut ws,
                            paths,
                            running,
                            registry,
                            mic_active,
                            speak,
                            tun,
                            endpoint_key.as_deref(),
                        );
                        registry.launch_detached();
                        result
                    }),
            },
            Some(Endpoint::Tcp(host)) => match std::net::TcpStream::connect(&host) {
                Ok(stream) => WsClient::handshake(stream, &format!("ws://{host}/"))
                    .and_then(|mut ws| {
                        ws.initialize(Duration::from_secs(10))?;
                        Ok(ws)
                    })
                    .map(|mut ws| {
                        registry.launch_ready(
                            remote_endpoint
                                .clone()
                                .expect("an endpoint produced this match arm"),
                        );
                        if launch_requested {
                            launch_kept_warm = true;
                        }
                        let result = run_attached(
                            &mut ws,
                            paths,
                            running,
                            registry,
                            mic_active,
                            speak,
                            tun,
                            endpoint_key.as_deref(),
                        );
                        registry.launch_detached();
                        result
                    }),
                Err(connect_error) => {
                    #[cfg(windows)]
                    if force_start && owned_server.is_none() && can_auto_start_tcp(&host) {
                        let codex_home = std::env::var_os("CODEX_HOME")
                            .filter(|s| !s.is_empty())
                            .map(PathBuf::from)
                            .unwrap_or_else(|| paths.codex_dir.clone());
                        let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
                        let bin = resolve_codex_bin(
                            &cfg.codex_bin,
                            &paths.home,
                            &codex_home,
                            roaming.as_deref(),
                        );
                        if let Some(bin) = bin {
                            warned_bin_unresolvable = false;
                            let throttled = last_server_start
                                .is_some_and(|at| at.elapsed() < SERVER_START_MIN_GAP);
                            if !throttled {
                                last_server_start = Some(Instant::now());
                                match start_tcp_app_server(&bin, &host) {
                                    Ok(child) => {
                                        owned_server = Some(child);
                                        log::info!(
                                            target: "engine",
                                            "codex-stream: started engine-owned app-server on ws://{host} client=codex"
                                        );
                                    }
                                    Err(e) => {
                                        if launch_requested {
                                            registry.launch_failed(e.clone());
                                        }
                                        log::info!(target: "engine", "codex-stream: {e} client=codex")
                                    }
                                }
                            }
                        } else {
                            if launch_requested {
                                registry.launch_failed(format!(
                                    "Codex executable {:?} was not found; set codex_bin to its full path",
                                    cfg.codex_bin
                                ));
                            }
                            if !warned_bin_unresolvable {
                                warned_bin_unresolvable = true;
                                log::info!(
                                    target: "engine",
                                    "codex-stream: app-server start is enabled but `{}` was not found — set codex_bin to the binary's full path client=codex",
                                    cfg.codex_bin
                                );
                            }
                        }
                    } else if force_start && !can_auto_start_tcp(&host) {
                        log::info!(
                            target: "engine",
                            "codex-stream: refusing to auto-start non-loopback ws://{host}; start and authenticate that app-server explicitly client=codex"
                        );
                    }
                    Err(format!("connect ws://{host}: {connect_error}"))
                }
            },
        };
        match attached {
            Ok(Ok(Detach::Shutdown)) => return,
            Ok(Ok(Detach::Disabled)) => {
                backoff = BACKOFF_FLOOR; // clean stand-down; the park above takes over
            }
            Ok(Ok(Detach::Reconfigure)) => {
                backoff = BACKOFF_FLOOR;
                continue;
            }
            Ok(Err(e)) => {
                // We WERE attached — but only a STABLE attachment earns the prompt
                // floor retry. A peer that accepts handshake+initialize and then drops
                // us immediately (version skew, misbehaving ws:// proxy) must pace like
                // an attach failure, or this arm is an unthrottled hot loop that also
                // writes this log line per iteration.
                let stable = attempt_started.elapsed() >= STABLE_ATTACH;
                backoff = next_backoff(stable, backoff);
                let delay = retry_delay(backoff, launch_requested);
                log::info!(
                    target: "engine",
                    "codex-stream: connection lost: {e}; reconnecting in {delay:?}"
                );
                pace(running, registry, delay, launch_requested);
            }
            Err(e) => {
                // Could not attach at all — quiet, common case (no daemon running).
                backoff = next_backoff(false, backoff);
                let delay = retry_delay(backoff, launch_requested);
                log::debug!(
                    target: "codex-stream",
                    "attach failed: {e}; next try in {delay:?}"
                );
                pace(running, registry, delay, launch_requested);
            }
        }
    }
}

/// Bounded inter-attempt wait that also yields immediately to a newly waiting launcher.
fn pace(
    running: &AtomicBool,
    registry: &SessionRegistry,
    delay: Duration,
    launch_was_requested: bool,
) {
    registry.wait_retry(running, delay, launch_was_requested);
}

#[cfg(test)]
mod tests;
