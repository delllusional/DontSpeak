//! Pid parsing (fail-closed) plus process lifecycle primitives.
//!
//! [`read_pid`] is the canonical pidfile codec; its consumer is `ds-config`'s
//! engine pidfile. Unix spawns can take their own session/group via
//! [`set_new_process_group`] and be reaped as a group; Windows has no `killpg`,
//! so [`kill_group`] there is a leaf `TerminateProcess`.

use std::fs;
use std::path::Path;

/// Make a Unix child the leader of a new session/process group before `exec`.
/// Callers can then terminate the complete child tree with [`kill_group`].
#[cfg(unix)]
pub fn set_new_process_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: pre_exec runs in the forked child before exec; setsid is
    // async-signal-safe, and the closure captures and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
        });
    }
}

/// Pidfile reader; `None` on any failure (stale/garbage fail-closed).
pub fn read_pid(pidfile: &Path) -> Option<i32> {
    let s = fs::read_to_string(pidfile).ok()?;
    let n: i32 = s.trim().parse().ok()?;
    if n > 0 { Some(n) } else { None }
}

// ---- platform: process-group liveness + kill -------------------------------

#[cfg(unix)]
mod imp {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;

    /// `kill(-pgid, 0)` — is the group still alive?
    pub fn group_alive(pgid: i32) -> bool {
        // signal 0 = existence check; Ok means the group exists & we may signal.
        killpg(Pid::from_raw(pgid), None).is_ok()
    }

    /// `kill -TERM -- -pgid`
    pub fn kill_group(pgid: i32) {
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGTERM);
    }

    /// Force-stop a process group that did not exit after [`kill_group`].
    pub fn force_kill_group(pgid: i32) {
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }

    /// Leaf-PID liveness (`kill(pid, 0)`). Ok or EPERM ⇒ alive; ESRCH ⇒ dead.
    /// Use this for the engine pidfile (plain pid), not [`group_alive`].
    pub fn pid_alive(pid: i32) -> bool {
        matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
    }

    /// SIGTERM a single process so the engine's own handler can reap the warm helper.
    pub fn terminate_pid(pid: i32) {
        let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
    }
}

#[cfg(windows)]
mod imp {
    // No killpg: callers pass a leaf PID. The children we terminate are
    // single-process (PowerShell System.Speech, ds-helper) — no Job Object yet.
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };

    pub fn group_alive(pid: i32) -> bool {
        // SAFETY: OpenProcess/CloseHandle take no pointers; handle closed before return.
        unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, pid as u32) {
                Ok(h) if !h.is_invalid() => {
                    let _ = CloseHandle(h);
                    true
                }
                _ => GetLastError() == ERROR_ACCESS_DENIED,
            }
        }
    }

    pub fn kill_group(pid: i32) {
        // SAFETY: `h` used only after OpenProcess Ok+valid; closed exactly once.
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid as u32)
                && !h.is_invalid()
            {
                let _ = TerminateProcess(h, 143);
                let _ = CloseHandle(h);
            }
        }
    }

    /// Windows has no graceful group signal; the first termination is already forced.
    pub fn force_kill_group(pid: i32) {
        kill_group(pid);
    }

    /// Leaf-PID liveness via QUERY_LIMITED_INFORMATION + STILL_ACTIVE (259).
    /// ACCESS_DENIED ⇒ alive (Windows analogue of unix EPERM). Deliberately not
    /// [`group_alive`], which opens with TERMINATE and reads denied as dead.
    pub fn pid_alive(pid: i32) -> bool {
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: `code` outlives GetExitCodeProcess; `h` closed once; GetLastError is TLS.
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) {
                Ok(h) if !h.is_invalid() => {
                    let mut code: u32 = 0;
                    let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE;
                    let _ = CloseHandle(h);
                    alive
                }
                _ => GetLastError() == ERROR_ACCESS_DENIED,
            }
        }
    }

    /// No graceful per-process signal on Windows — same leaf TerminateProcess as
    /// [`kill_group`]. Warm helper exits on stdin EOF when the engine pipe closes.
    pub fn terminate_pid(pid: i32) {
        kill_group(pid);
    }
}

pub use imp::{force_kill_group, group_alive, kill_group, pid_alive, terminate_pid};

// `mic_active` lives in ds-platform (OS boundary): use ds_platform::mic_active.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_written_pid_and_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        fs::write(&pf, "4242\n").unwrap();
        assert_eq!(read_pid(&pf), Some(4242));
        fs::remove_file(&pf).unwrap();
        assert_eq!(read_pid(&pf), None);
    }

    #[test]
    fn rejects_nonpositive_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("p");
        fs::write(&pf, "0").unwrap();
        assert_eq!(read_pid(&pf), None);
        fs::write(&pf, "-9").unwrap();
        assert_eq!(read_pid(&pf), None);
        fs::write(&pf, "notanum\n").unwrap();
        assert_eq!(read_pid(&pf), None);
    }
}
