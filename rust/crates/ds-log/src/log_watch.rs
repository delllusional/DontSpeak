//! Push-based logs-dir watch — client-side analogue of `dontspeakd::config_watch`,
//! but ephemeral: each `wait_logs_changed` spawns a watcher, blocks up to `timeout`, returns.
//! Callers loop (call → re-read `combined_log_json` → render). No `since` token — always
//! re-render the full tail. A change in the gap between return and the next attach is only
//! caught on the next write (acceptable for a human log viewer).

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Trailing-edge quiet window after the first relevant event — coalesces a burst into one
/// wake. Short on purpose (live view; config.toml's window is 750ms).
const LOG_WAIT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Block until `log_file`'s parent has a relevant `*.log` change (unified + aux; not
/// `*.log.N` — extension must be exactly `log`, same as `combined_log_json_at`) or
/// `timeout` elapses. Always returns; caller re-reads `combined_log_json` afterward.
pub fn wait_logs_changed(log_file: &Path, timeout: Duration) {
    wait_logs_changed_at(log_file.parent(), timeout);
}

fn wait_logs_changed_at(dir: Option<&Path>, timeout: Duration) {
    let Some(dir) = dir else {
        std::thread::sleep(timeout);
        return;
    };
    // First run may predate the logs dir (mirrors config_watch::spawn).
    let _ = std::fs::create_dir_all(dir);

    let (tx, rx) = mpsc::channel::<()>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if is_relevant(&event.kind) && touches_a_log_file(&event.paths) {
            let _ = tx.send(());
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        // Watcher init failed (e.g. inotify exhaustion): sleep once; next call retries.
        // Not a persistent poll timer.
        Err(_) => {
            std::thread::sleep(timeout);
            return;
        }
    };
    if watcher.watch(dir, RecursiveMode::NonRecursive).is_err() {
        std::thread::sleep(timeout);
        return;
    }

    let deadline = Instant::now() + timeout;
    if rx.recv_timeout(timeout).is_err() {
        return; // timed out with no event
    }
    // Debounce, capped by deadline so a hot log never exceeds the caller's timeout.
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = LOG_WAIT_DEBOUNCE.min(remaining);
        if wait.is_zero() || rx.recv_timeout(wait).is_err() {
            break;
        }
    }
    // `watcher` drops here — ephemeral by design (module doc).
}

/// Same gate as `dontspeakd::config_watch::is_relevant` (duplicated: cycle if shared via
/// `ds-config` ↔ `dontspeakd`). Access/Other ignored; Create/Modify/Remove pass.
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Path ends in `.log`? Matches `combined_log_json_at`'s aux filter so wake set and read set
/// never drift (excludes rotated `dontspeak.log.1` — extension is the numeral).
fn touches_a_log_file(paths: &[std::path::PathBuf]) -> bool {
    paths
        .iter()
        .any(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // No live FS event e2e here — same scope as config_watch (pure helpers only).
    // OS event delivery under `cargo test` child processes has been flaky in this workspace
    // (direct binary fine; cargo test sometimes never gets the callback). Not a bug in
    // wait_logs_changed_at. times_out_with_no_write only needs wall time.

    #[test]
    fn times_out_with_no_write() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        wait_logs_changed_at(Some(dir.path()), Duration::from_millis(200));
        assert!(Instant::now().duration_since(start) >= Duration::from_millis(200));
    }

    #[test]
    fn relevant_kinds_and_log_extension_filter_match_combined_log_json() {
        use notify::event::{AccessKind, CreateKind};
        assert!(is_relevant(&EventKind::Create(CreateKind::File)));
        assert!(!is_relevant(&EventKind::Access(AccessKind::Any)));
        assert!(touches_a_log_file(&[std::path::PathBuf::from(
            "/x/dontspeak.log"
        )]));
        assert!(touches_a_log_file(&[std::path::PathBuf::from(
            "/x/ds-helper.log"
        )]));
        assert!(!touches_a_log_file(&[std::path::PathBuf::from(
            "/x/dontspeak.log.1"
        )]));
    }
}
