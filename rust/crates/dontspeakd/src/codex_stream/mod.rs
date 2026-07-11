//! The Codex app-server SUBSCRIBER — mid-turn narration for OpenAI Codex sessions
//! (issue delllusional/DontSpeak#10). Codex has no `MessageDisplay` hook stream, so no
//! per-batch hook process can exist; instead the ENGINE (the one long-lived resident
//! process) attaches to the user's shared codex app-server (the daemon behind
//! `codex --remote`), subscribes to the threads that belong to REGISTERED DontSpeak
//! sessions, and translates `item/agentMessage/delta` / `item/completed` into the same
//! `ds_narrate::StreamBatch`es the Claude Code / Qwen Code hook adapters feed — one
//! shared core, three thin adapters (docs/STREAMING-NARRATION.md).
//!
//! Scope guarantees:
//!   * **Session-keyed, never narrate-everything** — only threads whose id maps
//!     ([`proto::session_for_thread`]) to a session the hooks registered (GreetSession /
//!     MarkActive over IPC) are resumed; a Codex Desktop / third-party thread on the same
//!     daemon is never narrated, and CC/Qwen session ids simply never match.
//!   * **Witness parity** — a successful `thread/resume` seeds the session's streaming
//!     witness ([`ds_narrate::seed_witness`]), so `Stop` stays silent for streamed
//!     sessions with ZERO changes to the Stop path; a plain-TUI session (no `--remote`)
//!     never resumes → no witness → `Stop` speaks exactly as today.
//!   * **Never double-speak** — every flush goes through [`ds_narrate::narrate_batch`],
//!     whose on-disk high-water mark makes reconnect/replay dedup-safe. On transient
//!     disconnects state files are KEPT (documented tradeoff: narration for turns during
//!     an app-server outage is lost rather than double-spoken).
//!   * **Cleanup is owned here** — Codex wires no `SessionEnd` hook, so the supervisor
//!     deletes the state/lock/tmp trio on eviction (thread gone from the daemon's loaded
//!     list, or a long idle TTL) and sweeps crash-orphaned state files at start.
//!
//! Config (`config.toml`, re-read per loop pass — no restart needed): `codex_stream`
//! (master switch, default on), `codex_stream_daemon_start` (opt-in lazy
//! `codex app-server daemon start`), `codex_app_server_url` (`ws://` TCP override — the
//! Windows path; the upstream daemon is Unix-only today), `codex_bin`.

mod client;
mod proto;

use std::collections::HashMap;
use std::io::{Read, Write};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ds_config::{NarrateKind, Paths, VoiceConfig};
use ds_narrate::{BatchPayload, StreamBatch};

use client::WsClient;

// ── The session registry (fed by the IPC hook arms, drained here) ────────────────

/// Session ids the hooks have told the engine about (`GreetSession` at SessionStart,
/// `MarkActive` at every prompt submit) — the ONLY ids the supervisor will ever resume
/// threads for. Every nudge bumps the epoch and wakes the supervisor, which re-arms
/// resolution for sessions that previously failed to match a loaded thread (the
/// "negative-cached until the next nudge" rule). Entries carry their last-nudge time so
/// long-dead ids (closed terminals — Codex has no SessionEnd hook) age out.
pub(crate) struct SessionRegistry {
    inner: Mutex<RegInner>,
    cv: Condvar,
}

struct RegInner {
    sessions: HashMap<String, Instant>,
    epoch: u64,
}

impl SessionRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(SessionRegistry {
            inner: Mutex::new(RegInner {
                sessions: HashMap::new(),
                epoch: 0,
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

    /// Park until the epoch moves past `seen` or `timeout` elapses; returns the current epoch.
    fn wait_change(&self, seen: u64, timeout: Duration) -> u64 {
        let g = self.inner.lock().unwrap();
        let (g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |g| g.epoch == seen)
            .unwrap();
        g.epoch
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

/// Per-(session, item) delta buffer: flush into `narrate_batch` on a newline, on age
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

/// Where to attach. Off-unix with no `ws://` override there is nothing to dial
/// (the upstream daemon is Unix-only today) — the caller parks.
pub(crate) enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(String),
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

/// `ws://host:port[/…]` → `host:port` for `TcpStream::connect`. Anything else → `None`.
pub(crate) fn parse_ws_url(url: &str) -> Option<String> {
    url.trim()
        .strip_prefix("ws://")
        .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
        .filter(|host| !host.is_empty())
}

/// Resolve the endpoint from config: a non-empty `codex_app_server_url` wins (TCP);
/// otherwise the default unix control socket (unix only).
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
        None
    }
}

/// PURE decision for the opt-in lazy daemon start — extracted so the shell-out itself is
/// never exercised in tests: start only when the user opted in, the socket is absent, and
/// a codex binary was actually resolved.
#[cfg(unix)]
pub(crate) fn should_start_daemon(
    daemon_start_enabled: bool,
    socket_present: bool,
    bin_resolved: bool,
) -> bool {
    daemon_start_enabled && !socket_present && bin_resolved
}

/// Resolve the codex binary: an absolute config path is used as-is; a bare name is
/// searched on PATH, then the common install dirs (a GUI-launched app has a minimal
/// PATH), including the standalone managed install under the codex home — the SAME
/// `$CODEX_HOME`-or-`paths.codex_dir` resolution [`control_socket_path`] uses, so the
/// binary lookup and the socket can't disagree about where codex lives. Unix-only like
/// the daemon start itself (the upstream daemon has no Windows lifecycle management).
#[cfg(unix)]
fn resolve_codex_bin(cfg_bin: &str, home: &Path, codex_home: &Path) -> Option<PathBuf> {
    let p = Path::new(cfg_bin);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(cfg_bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let fallbacks = [
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        home.join(".local/bin"),
        codex_home.join("packages/standalone/current"),
    ];
    for dir in fallbacks {
        let candidate = dir.join(cfg_bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Launch `codex app-server daemon start` (idempotent upstream; returns once the control
/// socket answers `initialize`). We never own or kill the DAEMON itself — an external
/// tool shell-out, same discipline as espeak-ng; codex is never linked. The short-lived
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
                "codex-stream: launched `{} app-server daemon start` (socket was absent)",
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

enum Pending {
    LoadedList,
    Resume { thread_id: String, session: String },
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
    speak: &mut dyn FnMut(&str, String),
    tun: &Tunables,
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
            .values()
            .any(|st| !st.negative && st.next_try <= now);
        let relist_due = last_list.is_none_or(|at| now.duration_since(at) >= tun.relist);
        if !list_in_flight && (unresolved_due || relist_due) {
            let id = ws.next_id();
            ws.send(proto::thread_loaded_list_request(id))?;
            pending.insert(id, Pending::LoadedList);
            list_in_flight = true;
            last_list = Some(now);
        }

        // One read tick (the socket read timeout paces this loop).
        match ws.read_text()? {
            None => {}
            Some(text) => match proto::parse_incoming(&text) {
                proto::Incoming::Response { id, result } => match pending.remove(&id) {
                    Some(Pending::LoadedList) => {
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
                                let wanted = resolve.contains_key(&session)
                                    && !resumed.contains_key(thread_id);
                                if wanted {
                                    let id = ws.next_id();
                                    ws.send(proto::thread_resume_request(id, thread_id))?;
                                    pending.insert(
                                        id,
                                        Pending::Resume {
                                            thread_id: thread_id.clone(),
                                            session,
                                        },
                                    );
                                }
                            }
                            // Sessions still unmatched: schedule the retry / negative-cache.
                            for (session, st) in resolve.iter_mut() {
                                let matched = loaded
                                    .iter()
                                    .any(|t| &proto::session_for_thread(t) == session);
                                if !matched && !st.negative {
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
                    Some(Pending::Resume { thread_id, session }) => match result {
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
                    if let Some(r) = resumed.get(&thread_id) {
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
    }
}

/// One batch through the shared core: the narrate gate, the file-backed step, the
/// per-utterance forward. `cfg` is the supervisor's (periodically refreshed) config read.
fn flush(
    paths: &Paths,
    cfg: &VoiceConfig,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, String),
    session: &str,
    batch: &StreamBatch,
) {
    let digests_on = cfg.narrates(NarrateKind::Digests);
    let shorts_on = cfg.narrates(NarrateKind::Shorts);
    if !digests_on && !shorts_on {
        return;
    }
    for utt in ds_narrate::narrate_batch(paths, session, batch, mic_active(), digests_on, shorts_on)
    {
        speak(session, utt);
    }
}

// ── The supervisor thread ─────────────────────────────────────────────────────────

/// Reconnect backoff bounds (1 s → 30 s, doubling).
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CEIL: Duration = Duration::from_secs(30);
/// An attachment must have held this long for its loss to earn the prompt floor retry;
/// anything shorter is treated like an attach failure (doubling backoff). Gates the
/// hot-loop where a peer keeps accepting handshake+initialize and then dropping us
/// (version-skew server, misbehaving ws:// proxy) — see [`next_backoff`].
const STABLE_ATTACH: Duration = Duration::from_secs(60);

/// PURE backoff step for the reconnect loop (unit-tested): a STABLE attachment's loss
/// resets to the floor; an unstable one (or a plain attach failure) doubles toward the
/// ceiling. The caller always sleeps the RETURNED delay before the next attempt — there
/// is no zero-sleep path, so no failure mode can spin the loop hot.
fn next_backoff(stable_attachment: bool, backoff: Duration) -> Duration {
    if stable_attachment {
        BACKOFF_FLOOR
    } else {
        (backoff * 2).min(BACKOFF_CEIL)
    }
}
/// Never shell out `daemon start` more often than this.
#[cfg(unix)]
const DAEMON_START_MIN_GAP: Duration = Duration::from_secs(60);

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
            // per-session hold/active routing, pool voices, and scoped barge all apply
            // because the session id matches the one the hooks use.
            let mut speak = move |session: &str, text: String| {
                ttsq.enqueue(text, None, None, Some(session.to_string()));
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
/// nudging the idempotent daemon start when opted in), then hand the connection to
/// [`run_attached`]; reconnect with capped backoff on disconnect.
fn supervise(
    paths: &Paths,
    running: &AtomicBool,
    registry: &SessionRegistry,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, String),
    tun: &Tunables,
) {
    let mut backoff = BACKOFF_FLOOR;
    let mut epoch = 0u64;
    #[cfg(unix)]
    let mut last_daemon_start: Option<Instant> = None;
    // One WARN per unresolvable-binary streak: this branch re-runs every backoff pass,
    // and an explicit opt-in that silently no-ops (nvm/npm-prefix installs off the GUI
    // PATH) is the failure the audit flagged — but a per-pass WARN would spam the log.
    #[cfg(unix)]
    let mut warned_bin_unresolvable = false;
    while running.load(Ordering::Relaxed) {
        let cfg = VoiceConfig::load(paths);
        let (sessions, cur_epoch) = registry.snapshot();
        if !cfg.codex_stream || !paths.codex_dir.exists() || sessions.is_empty() {
            // Parked. A nudge (or the timeout) re-checks the gate.
            epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_secs(5));
            continue;
        }
        let endpoint = resolve_endpoint(
            &cfg.codex_app_server_url,
            std::env::var_os("CODEX_HOME").as_deref(),
            paths,
        );
        let attempt_started = Instant::now();
        let attached: Result<Result<Detach, String>, String> = match endpoint {
            None => {
                // No dialable endpoint on this platform (Windows without a ws:// override)
                // — Stop-only narration stands. Park until config changes.
                epoch = registry.wait_change(cur_epoch.max(epoch), Duration::from_secs(30));
                continue;
            }
            #[cfg(unix)]
            Some(Endpoint::Unix(sock)) => {
                if !sock.exists() {
                    let codex_home = std::env::var_os("CODEX_HOME")
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| paths.codex_dir.clone());
                    let bin = resolve_codex_bin(&cfg.codex_bin, &paths.home, &codex_home);
                    if cfg.codex_stream_daemon_start && bin.is_none() {
                        if !warned_bin_unresolvable {
                            warned_bin_unresolvable = true;
                            log::info!(
                                target: "engine",
                                "codex-stream: daemon start is enabled but `{}` was not found on PATH or the known install dirs — set codex_bin to the binary's full path client=codex",
                                cfg.codex_bin
                            );
                        }
                    } else {
                        warned_bin_unresolvable = false;
                    }
                    let throttled =
                        last_daemon_start.is_some_and(|at| at.elapsed() < DAEMON_START_MIN_GAP);
                    if !throttled
                        && should_start_daemon(
                            cfg.codex_stream_daemon_start,
                            sock.exists(),
                            bin.is_some(),
                        )
                        && let Some(bin) = bin
                    {
                        last_daemon_start = Some(Instant::now());
                        start_daemon(&bin);
                    }
                    Err(format!("control socket absent: {}", sock.display()))
                } else {
                    std::os::unix::net::UnixStream::connect(&sock)
                        .map_err(|e| format!("connect {}: {e}", sock.display()))
                        .and_then(WsClient::handshake)
                        .and_then(|mut ws| {
                            ws.initialize(Duration::from_secs(10))?;
                            Ok(ws)
                        })
                        .map(|mut ws| {
                            run_attached(&mut ws, paths, running, registry, mic_active, speak, tun)
                        })
                }
            }
            Some(Endpoint::Tcp(host)) => std::net::TcpStream::connect(&host)
                .map_err(|e| format!("connect ws://{host}: {e}"))
                .and_then(WsClient::handshake)
                .and_then(|mut ws| {
                    ws.initialize(Duration::from_secs(10))?;
                    Ok(ws)
                })
                .map(|mut ws| {
                    run_attached(&mut ws, paths, running, registry, mic_active, speak, tun)
                }),
        };
        match attached {
            Ok(Ok(Detach::Shutdown)) => return,
            Ok(Ok(Detach::Disabled)) => {
                backoff = BACKOFF_FLOOR; // clean stand-down; the park above takes over
            }
            Ok(Err(e)) => {
                // We WERE attached — but only a STABLE attachment earns the prompt
                // floor retry. A peer that accepts handshake+initialize and then drops
                // us immediately (version skew, misbehaving ws:// proxy) must pace like
                // an attach failure, or this arm is an unthrottled hot loop that also
                // writes this log line per iteration.
                let stable = attempt_started.elapsed() >= STABLE_ATTACH;
                backoff = next_backoff(stable, backoff);
                log::info!(
                    target: "engine",
                    "codex-stream: connection lost: {e}; reconnecting in {backoff:?}"
                );
                pace(running, backoff);
            }
            Err(e) => {
                // Could not attach at all — quiet, common case (no daemon running).
                backoff = next_backoff(false, backoff);
                log::debug!(
                    target: "codex-stream",
                    "attach failed: {e}; next try in {backoff:?}"
                );
                pace(running, backoff);
            }
        }
    }
}

/// Bounded inter-attempt sleep that still observes shutdown.
fn pace(running: &AtomicBool, delay: Duration) {
    let deadline = Instant::now() + delay;
    while running.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests;
