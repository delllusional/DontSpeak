//! Engine pidfile (§E.4 hot-reload) — read/parse half (PURE) + liveness probe +
//! the single-instance guard.
//!
//! `dontspeakd` writes `std::process::id()` to `paths.engine_pid` on startup so the
//! GUI can SIGHUP it for a no-restart reload + probe its liveness, and so a NEWLY
//! starting engine can evict an older one ([`evict_stale_engine`]). The PARSE is
//! pure (no signals, no processes) so it unit-tests on a tempdir file; the
//! liveness/terminate primitives are the cross-platform ones in `ds_proc`.

/// Read the engine pid recorded in `path`, if present and well-formed.
///
/// PURE: returns `None` on ANY failure (missing file, empty, garbage,
/// non-positive) so a stale/garbage pidfile never yields a bogus pid the caller
/// might signal. No signals, no processes. Delegates to the canonical pidfile
/// codec [`ds_proc::read_pid`] so the read/parse rule lives in one place.
pub fn read_engine_pid(path: &std::path::Path) -> Option<i32> {
    ds_proc::read_pid(path)
}

/// Is the pid recorded in the engine pidfile still alive? Reads + parses the
/// pidfile (PURE half) then probes liveness. Returns false on a missing/garbage
/// pidfile OR a dead pid — so the GUI never SIGHUPs a stale pid that the OS may
/// have recycled to an unrelated process. Cross-platform via [`pid_alive`].
pub fn engine_pid_alive(path: &std::path::Path) -> bool {
    match read_engine_pid(path) {
        Some(pid) => pid_alive(pid),
        None => false,
    }
}

/// Is a single process alive? Delegates to [`ds_proc::pid_alive`] — the
/// EPERM-means-alive (unix) / QUERY_LIMITED_INFORMATION-or-access-denied (windows)
/// probe, so this contract lives in ONE place across the platforms.
pub fn pid_alive(pid: i32) -> bool {
    ds_proc::pid_alive(pid)
}

/// Single-instance guard: evict an OLDER engine before this one binds the socket.
///
/// There is no portable OS singleton we can lean on — macOS's launchd `KeepAlive`
/// only covers a launchd-managed daemon, NOT the engine that runs IN-PROCESS inside
/// the GUI host, and the Windows/headless paths have none at all. Worse,
/// `ds_ipc::bind` unlinks + rebinds the socket, so a second engine STEALS the
/// path from a still-running first one instead of failing — leaving TWO engines that
/// both narrate, which is heard as the same reply spoken twice after a reinstall or
/// upgrade. So a starting engine reads the recorded pid and, if it is a DIFFERENT
/// live process, asks it to exit first: SIGTERM on unix (the old engine's handler
/// runs its clean shutdown, reaping its warm helper); `TerminateProcess` on Windows
/// (the old helper then self-exits on stdin EOF). Returns the pid evicted, if any.
/// Never targets our own pid, and is a no-op when the recorded engine is already gone.
pub fn evict_stale_engine(path: &std::path::Path, self_pid: u32) -> Option<i32> {
    let pid = read_engine_pid(path)?;
    if pid as u32 == self_pid || !pid_alive(pid) {
        return None;
    }
    ds_proc::terminate_pid(pid);
    Some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Engine pidfile read/parse/stale (§E.4) ──────────────────────────────
    // PURE: a tempdir file, no signals, no processes.

    #[test]
    fn read_engine_pid_parses_well_formed() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        // Mirrors the `fs::write(getpid())` the engine does, with a trailing
        // newline an editor might add — trim handles it.
        std::fs::write(&pf, "12345\n").unwrap();
        assert_eq!(read_engine_pid(&pf), Some(12345));
        // No trailing newline also parses.
        std::fs::write(&pf, "678").unwrap();
        assert_eq!(read_engine_pid(&pf), Some(678));
    }

    #[test]
    fn read_engine_pid_rejects_garbage_empty_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        // Missing file → None (never a bogus pid to signal).
        assert_eq!(read_engine_pid(&pf), None);
        // Empty / whitespace-only → None.
        std::fs::write(&pf, "").unwrap();
        assert_eq!(read_engine_pid(&pf), None);
        std::fs::write(&pf, "   \n").unwrap();
        assert_eq!(read_engine_pid(&pf), None);
        // Non-numeric garbage → None.
        std::fs::write(&pf, "not-a-pid").unwrap();
        assert_eq!(read_engine_pid(&pf), None);
        // Non-positive pids are rejected (0 / negative are never valid engine pids).
        std::fs::write(&pf, "0").unwrap();
        assert_eq!(read_engine_pid(&pf), None);
        std::fs::write(&pf, "-7").unwrap();
        assert_eq!(read_engine_pid(&pf), None);
    }

    #[test]
    fn engine_pid_alive_false_on_missing_and_garbage() {
        // The liveness probe over a missing/garbage pidfile is false WITHOUT
        // ever signalling anything (read_engine_pid returns None first).
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        assert!(!engine_pid_alive(&pf));
        std::fs::write(&pf, "garbage").unwrap();
        assert!(!engine_pid_alive(&pf));
    }

    #[test]
    fn evict_stale_engine_is_noop_for_self_missing_and_dead() {
        // Never targets our own pid; no-op on a missing pidfile or a dead recorded pid.
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        let me = std::process::id();
        assert_eq!(evict_stale_engine(&pf, me), None); // missing pidfile
        std::fs::write(&pf, me.to_string()).unwrap();
        assert_eq!(evict_stale_engine(&pf, me), None); // recorded == self
        std::fs::write(&pf, i32::MAX.to_string()).unwrap();
        assert_eq!(evict_stale_engine(&pf, me), None); // recorded is dead
    }

    #[test]
    fn pid_alive_true_for_self_false_for_stale() {
        // Our own pid is alive; a very high unused pid is not (a stale pidfile
        // must never report alive, so the GUI never SIGHUPs a recycled pid).
        let me = std::process::id() as i32;
        assert!(pid_alive(me));
        // i32::MAX is not a live pid on any sane system.
        assert!(!pid_alive(i32::MAX));
    }
}
