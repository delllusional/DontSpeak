//! Push-based logs-dir watch — the client-side analogue of
//! `dontspeakd::config_watch` (that one watches settings.json's parent dir; this
//! one watches the logs dir), but with no persistent state: `wait_logs_changed`
//! spawns an EPHEMERAL watcher per call, blocks up to `timeout`, and returns.
//! Callers loop: call → re-read `combined_log_json` → render → call again
//! (mirrors how `ds_model_status_wait` is looped, but with no `since` token —
//! the caller never diffs content, it always re-renders the full current tail,
//! so a change landing in the tiny gap between one call returning and the next
//! call's watcher attaching is only caught on the NEXT write — acceptable for a
//! human-facing log viewer, not correctness-critical).

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::Paths;

/// Trailing-edge quiet window: once the first relevant event lands, keep
/// draining for this long before returning, so a burst of rapid appends (many
/// log lines in one flush) settles into one wake instead of one per line. Short
/// on purpose — this is a live log view, not settings.json (whose own
/// `RELOAD_QUIET_WINDOW` is 750ms); logs should feel closer to instant.
const LOG_WAIT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Block until the logs directory (`paths.log_file`'s parent) has a relevant
/// change — any `*.log` file created/modified/removed, including the unified
/// log itself and every sibling aux log, but excluding rotated `*.log.N`
/// (same predicate `combined_log_json_at` already uses: extension must be
/// exactly `log`) — or until `timeout` elapses. Always returns; the caller
/// re-reads `combined_log_json` unconditionally afterward (same content on a
/// timeout as before the call, which is harmless).
pub fn wait_logs_changed(paths: &Paths, timeout: Duration) {
    wait_logs_changed_at(paths.log_file.parent(), timeout);
}

fn wait_logs_changed_at(dir: Option<&Path>, timeout: Duration) {
    let Some(dir) = dir else { return };
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
        // Graceful fallback: the watcher couldn't init (e.g. inotify fd
        // exhaustion). Sleep out the timeout so the caller's loop just re-reads
        // on its own cadence this one time — NOT a persistent poll timer, since
        // the very next call retries a fresh watcher.
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
    // Trailing-edge debounce, capped by the overall deadline so a hot log never
    // blows past the caller's requested timeout.
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = LOG_WAIT_DEBOUNCE.min(remaining);
        if wait.is_zero() || rx.recv_timeout(wait).is_err() {
            break;
        }
    }
    // `watcher` drops here — ephemeral by design (see module doc).
}

/// Same relevance gate as `dontspeakd::config_watch::is_relevant` (duplicated,
/// not shared: `ds-config` cannot depend on `dontspeakd`, which depends on
/// `ds-config`) — a pure `Access` or the catch-all `Other` can't reflect a log
/// write; `Create`/`Modify`/`Remove` all pass.
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Does any path in the event end in `.log`? Matches `combined_log_json_at`'s
/// own aux-file filter exactly, so "what wakes the watcher" and "what the next
/// read returns" never drift: this covers the unified log (`dontspeak.log`) AND
/// every sibling aux log, and naturally excludes rotated `dontspeak.log.1`/`.2`
/// (their extension is the numeral, not `log`).
fn touches_a_log_file(paths: &[std::path::PathBuf]) -> bool {
    paths
        .iter()
        .any(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test exercises a real OS-delivered fs event end-to-end here, on purpose —
    // matching `dontspeakd::config_watch`'s own test scope (it only tests its pure
    // `is_relevant`/`event_touches_config` helpers, never a live cross-thread FSEvents
    // round trip). Real event delivery timing is an OS/CFRunLoop concern outside this
    // process's control and was confirmed flaky specifically under some `cargo test`
    // child-process invocations in this workspace (a manually compiled + directly
    // executed binary reliably saw the write in ~10-270ms; the identical code spawned
    // as a `cargo test` child sometimes never received a callback at all) — a real
    // dependency on OS-level event delivery, not a bug in `wait_logs_changed_at`
    // itself. `times_out_with_no_write` below only depends on elapsed wall time, not
    // on any event arriving, so it stays reliable.

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
