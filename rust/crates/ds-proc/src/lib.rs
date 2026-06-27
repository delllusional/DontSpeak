//! Single-speaker arbitration + barge-in for dontspeak.
//!
//! The contract (shared with the engine's caps-ON barge-in):
//!   * `~/.claude/speak-hook.pid` holds the **process-GROUP id** of the current
//!     speaker. On unix the speaker runs in its own process group (the shell
//!     used `set -m`; our Rust executor calls `setsid`/`setpgid` so the spawned
//!     `uv`/`python`/`afplay` tree shares one pgid).
//!   * To preempt, send SIGTERM to the **negative** pgid (`killpg`) so the whole
//!     tree dies — mirrors `kill -TERM -- -<pgid>`.
//!   * The pidfile is written **atomically** (tempfile in the same dir + rename)
//!     so a reader never sees a half-written value.
//!
//! Windows has no POSIX process groups; the cfg'd impl single-PID
//! `OpenProcess`/`TerminateProcess`es the recorded leaf PID (details in the
//! `cfg(windows)` mod). UNCOMPILED on the macOS build host.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

/// Canonical pidfile reader: read the file, trim, parse a positive `i32`.
///
/// PURE: returns `None` on ANY failure (missing file, empty, garbage,
/// non-positive) so a stale/garbage pidfile never yields a bogus pid a caller
/// might signal. This is the single home of the pidfile codec — both the speaker
/// pidfile and ds-config's engine pidfile read through it.
pub fn read_pid(pidfile: &Path) -> Option<i32> {
    let s = fs::read_to_string(pidfile).ok()?;
    let n: i32 = s.trim().parse().ok()?;
    if n > 0 { Some(n) } else { None }
}

/// Atomically write `pgid` to the pidfile (tempfile in the same dir + rename).
pub fn write_speaker(pidfile: &Path, pgid: i32) -> std::io::Result<()> {
    let dir: PathBuf = pidfile
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir)?;
    let mut tmp = NamedTempFile::new_in(&dir)?;
    write!(tmp, "{pgid}")?;
    tmp.flush()?;
    // persist() = atomic rename onto the final path.
    tmp.persist(pidfile).map_err(|e| e.error)?;
    Ok(())
}

/// Record an ALREADY-spawned speaker's pgid in the pidfile, or kill it.
///
/// The SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2): the pgid
/// MUST be recorded before the speaker makes sound, else a later spawn could
/// overwrite the pidfile and two speakers play at once. On write failure we kill
/// the just-spawned group and propagate the error, so a spawn that can't be
/// tracked never leaves an orphan sounding. Returns the recorded pid on success.
///
/// The caller owns the `Child` (drop it to wait by pgid, or keep it to wait
/// directly) — this only writes-or-kills.
pub fn record_or_kill(pidfile: &Path, child: &std::process::Child) -> std::io::Result<i32> {
    // setsid => the child is its own group leader, so pgid == pid.
    let pid = child.id() as i32;
    if let Err(e) = write_speaker(pidfile, pid) {
        kill_group(pid);
        return Err(e);
    }
    Ok(pid)
}

/// Best-effort removal of the pidfile (speaker finished).
pub fn clear_speaker(pidfile: &Path) {
    let _ = fs::remove_file(pidfile);
}

/// Preempt the speaker recorded in the pidfile, if it is still alive.
/// Returns the pgid that was signalled, or None if there was nothing to kill.
pub fn barge_in(pidfile: &Path) -> Option<i32> {
    let pgid = read_pid(pidfile)?;
    if group_alive(pgid) {
        kill_group(pgid);
        Some(pgid)
    } else {
        None
    }
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

    /// Is a SINGLE process alive? `kill(pid, 0)` — Ok ⇒ exists & signalable; EPERM
    /// ⇒ exists but not ours to signal (still ALIVE). Any other errno (ESRCH) ⇒ dead.
    /// Unlike [`group_alive`] this is the leaf-PID probe (no pgid), so it is the right
    /// liveness test for the engine pidfile, whose recorded id is a plain pid.
    pub fn pid_alive(pid: i32) -> bool {
        matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
    }

    /// SIGTERM a SINGLE process (not its group), so the engine's own signal handler
    /// runs its clean shutdown (which reaps the warm helper) rather than the helper
    /// being torn out from under it.
    pub fn terminate_pid(pid: i32) {
        let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
    }
}

#[cfg(windows)]
mod imp {
    // UNCOMPILED on the macOS build host.
    //
    // Windows has no killpg. The "pgid" stored in the pidfile is the speaker's
    // leaf PID; barge-in does a single-PID OpenProcess/TerminateProcess on it.
    // Current Windows speakers (inline PowerShell System.Speech; native
    // in-process ds-helper) are single-process, so killing the leaf PID
    // tears down the whole speaker — no Job Object is needed yet.
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };

    pub fn group_alive(pid: i32) -> bool {
        unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, pid as u32) {
                Ok(h) if !h.is_invalid() => {
                    let _ = CloseHandle(h);
                    true
                }
                _ => false,
            }
        }
    }

    pub fn kill_group(pid: i32) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid as u32) {
                if !h.is_invalid() {
                    let _ = TerminateProcess(h, 143);
                    let _ = CloseHandle(h);
                }
            }
        }
    }

    /// Is a SINGLE process alive? Open with QUERY_LIMITED_INFORMATION (the least
    /// right that still works on processes we don't own) and read its exit code:
    /// `STILL_ACTIVE` (259) ⇒ running. If the open is DENIED the process exists but
    /// is not openable by us ⇒ treat as alive (the Windows analogue of unix EPERM,
    /// honoring the same "permission error means alive" contract); any other failure
    /// (no such pid) ⇒ dead. Deliberately NOT `group_alive`, which opens with
    /// PROCESS_TERMINATE and reads access-denied as DEAD — wrong for a liveness probe.
    pub fn pid_alive(pid: i32) -> bool {
        const STILL_ACTIVE: u32 = 259;
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

    /// Terminate a SINGLE process. Windows has no graceful per-process signal, so
    /// this is the same leaf-PID `TerminateProcess` as [`kill_group`]; the warm
    /// helper child is not orphaned because it exits on its stdin EOF when the
    /// engine's pipe write-end closes (see ds_helper serve loop).
    pub fn terminate_pid(pid: i32) {
        kill_group(pid);
    }
}

pub use imp::{group_alive, kill_group, pid_alive, terminate_pid};

// `mic_active` (the CoreAudio TTS-feedback gate) lives in ds-platform, the OS
// boundary; use ds_platform::mic_active.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("speak-hook.pid");
        write_speaker(&pf, 4242).unwrap();
        assert_eq!(read_pid(&pf), Some(4242));
        clear_speaker(&pf);
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

    #[test]
    fn barge_in_none_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(barge_in(&dir.path().join("missing.pid")), None);
    }

    #[test]
    fn record_or_kill_writes_pid_and_returns_it() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("speak-hook.pid");
        // A short-lived child whose pid we can record; portable across unix and Windows.
        #[cfg(unix)]
        let mut child = std::process::Command::new("true").spawn().unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let expected = child.id() as i32;
        let pid = record_or_kill(&pf, &child).unwrap();
        assert_eq!(pid, expected);
        assert_eq!(read_pid(&pf), Some(expected));
        let _ = child.wait();
    }
}
