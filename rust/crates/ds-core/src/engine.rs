//! Engine running-probe.
//!
//! The engine (`dontspeakd`) runs in-process in the host app (started by
//! `ds_engine_start`) and writes its pidfile (`Paths::engine_pid`). Liveness is that
//! heartbeat; config reload is the in-process flag from `ds_engine_reload` (not a signal).
//! Read failures → silent `false`.

/// Best-effort engine liveness via pidfile. Cross-platform: `ds_config::is_engine_pid_alive`
/// uses `kill(pid, 0)` on unix and `OpenProcess`+`GetExitCodeProcess` on Windows
/// (access-denied = alive — same EPERM contract; not `ds_proc::group_alive`, which
/// treats access-denied as dead).
pub fn is_running() -> bool {
    ds_config::Paths::resolve()
        .map(|paths| ds_config::is_engine_pid_alive(&paths.engine_pid))
        .unwrap_or(false)
}
