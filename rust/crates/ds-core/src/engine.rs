//! Engine liveness via pidfile (`Paths::engine_pid`). Read failure → false.

/// Pidfile probe. Access-denied = alive (EPERM); not `ds_proc::group_alive`.
pub fn is_running() -> bool {
    ds_config::Paths::resolve()
        .map(|paths| ds_config::is_engine_pid_alive(&paths.engine_pid))
        .unwrap_or(false)
}
