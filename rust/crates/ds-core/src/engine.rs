//! Engine running-probe.
//!
//! The engine (`dontspeakd`) runs IN-PROCESS inside the resident host app (macOS
//! DontSpeak.app / Windows ds-winui / Linux ds-gtk) — started on a background
//! thread by the FFI `ds_engine_start` (see `ffi.rs`) — and writes
//! its own pidfile (`~/.claude/dontspeakd.pid`, via `ds_config::Paths::engine_pid`).
//! Liveness is that pidfile heartbeat; config reload is the in-process atomic flag
//! the FFI sets (`ds_engine_reload`), not a signal or launchctl — so there is
//! nothing to do here for it. Degrade quietly: any read failure is a silent `false`,
//! never a crash.

/// Is the engine currently running (best-effort)? The recorded pid's liveness;
/// `false` when no pidfile exists. Cross-platform: `ds_config::engine_pid_alive`
/// probes `kill(pid, 0)` on unix and `OpenProcess(QUERY_LIMITED_INFORMATION)` +
/// `GetExitCodeProcess` on Windows (access-denied read as alive, honoring the same
/// EPERM-means-alive contract — NOT `ds_proc::group_alive`, which would read
/// access-denied as dead).
pub fn is_running() -> bool {
    ds_config::Paths::resolve()
        .map(|paths| ds_config::engine_pid_alive(&paths.engine_pid))
        .unwrap_or(false)
}
