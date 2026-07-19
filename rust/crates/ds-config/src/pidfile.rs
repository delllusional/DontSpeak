//! Engine pidfile — parse + liveness + single-instance guard ([`evict_stale_engine`]).
//! Liveness alone ≠ identity (PID recycle) — eviction confirms same exe.

/// Pure parse; `None` on failure (no bogus pid to signal). Via [`ds_proc::read_pid`].
pub fn read_engine_pid(path: &std::path::Path) -> Option<i32> {
    ds_proc::read_pid(path)
}

/// Pidfile pid still alive? False on missing/garbage/dead.
pub fn is_engine_pid_alive(path: &std::path::Path) -> bool {
    match read_engine_pid(path) {
        Some(pid) => is_pid_alive(pid),
        None => false,
    }
}

/// Process alive? Via [`ds_proc::pid_alive`] (EPERM-means-alive / access-denied).
pub fn is_pid_alive(pid: i32) -> bool {
    ds_proc::pid_alive(pid)
}

/// Best-effort exe basename for a live `pid`. Callers MUST treat `None` as unconfirmed
/// (fail closed — no signal).
#[cfg(target_os = "linux")]
fn exe_basename_for_pid(pid: i32) -> Option<String> {
    let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    target.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// macOS: `ps -o comm=` (no `/proc`).
#[cfg(target_os = "macos")]
fn exe_basename_for_pid(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("comm=")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let name = stdout.trim();
    if name.is_empty() {
        return None;
    }
    // macOS may print full path; compare basenames like Linux/Windows.
    Some(
        std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string()),
    )
}

/// Windows: `tasklist` image name.
#[cfg(windows)]
fn exe_basename_for_pid(pid: i32) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("tasklist");
    cmd.arg("/FI")
        .arg(format!("PID eq {pid}"))
        .arg("/FO")
        .arg("CSV")
        .arg("/NH")
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // CSV: "image.exe","1234",… — first field is the name.
    let first_field = stdout.lines().next()?.trim().split(',').next()?;
    let name = first_field.trim().trim_matches('"');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Other targets: identity unconfirmed.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn exe_basename_for_pid(_pid: i32) -> Option<String> {
    None
}

/// Same exe basename? Closes PID-recycle hole; false on uncertainty.
fn is_same_engine_binary(pid: i32) -> bool {
    let Some(want) = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    else {
        return false;
    };
    exe_basename_for_pid(pid).is_some_and(|got| got.eq_ignore_ascii_case(&want))
}

/// Evict an older same-binary engine before binding the socket.
///
/// No portable singleton; GUI hosts engine in-process. `ds_ipc::bind` rebinds, so a second
/// start steals the socket → double narration. Signal only when live pid is confirmed same
/// exe; otherwise drop garbage pidfile. Skips self.
pub fn evict_stale_engine(path: &std::path::Path, self_pid: u32) -> Option<i32> {
    let pid = read_engine_pid(path)?;
    if pid as u32 == self_pid || !is_pid_alive(pid) {
        return None;
    }
    if !is_same_engine_binary(pid) {
        // Alive but identity unconfirmed (likely recycled) — drop pidfile, no signal.
        let _ = std::fs::remove_file(path);
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
        assert!(!is_engine_pid_alive(&pf));
        std::fs::write(&pf, "garbage").unwrap();
        assert!(!is_engine_pid_alive(&pf));
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
    fn evict_stale_engine_refuses_live_pid_with_mismatched_identity() {
        // PID recycle: live pid, different exe → drop pidfile, no signal.
        // Linux CI can recycle a short-lived child's pid/tid onto this test binary
        // (`/proc/<tid>/exe` = parent); use `sleep` and retry; skip if unprovable.
        let mut other_pid = 0;
        let mut child = None;
        for attempt in 1..=5 {
            #[cfg(unix)]
            let mut candidate = std::process::Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            #[cfg(windows)]
            let mut candidate = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let candidate_pid = candidate.id() as i32;
            if is_pid_alive(candidate_pid) && !is_same_engine_binary(candidate_pid) {
                other_pid = candidate_pid;
                child = Some(candidate);
                break;
            }
            eprintln!(
                "attempt {attempt}/5: freshly spawned pid {candidate_pid} resolved to our \
                 own exe (resolved to {:?}) — retrying with a new child",
                exe_basename_for_pid(candidate_pid)
            );
            let _ = candidate.kill();
            let _ = candidate.wait();
        }
        let Some(mut child) = child else {
            eprintln!(
                "skipping evict_stale_engine_refuses_live_pid_with_mismatched_identity: \
                 could not get a freshly spawned pid with a distinct identity in 5 \
                 attempts — this environment's pid/tid churn made the precondition \
                 unprovable, not a failure of evict_stale_engine itself"
            );
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("dontspeakd.pid");
        std::fs::write(&pf, other_pid.to_string()).unwrap();
        let me = std::process::id();

        assert_eq!(evict_stale_engine(&pf, me), None);
        assert!(!pf.exists(), "garbage pidfile must be removed");
        assert!(
            is_pid_alive(other_pid),
            "the unrelated live process must NOT have been signalled"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn pid_alive_true_for_self_false_for_stale() {
        // Our own pid is alive; a very high unused pid is not (a stale pidfile
        // must never report alive, so the guard never targets a recycled pid).
        let me = std::process::id() as i32;
        assert!(is_pid_alive(me));
        // i32::MAX is not a live pid on any sane system.
        assert!(!is_pid_alive(i32::MAX));
    }
}
