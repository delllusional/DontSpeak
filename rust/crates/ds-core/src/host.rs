//! In-process engine host — sole owner of the background engine thread for the C ABI
//! ([`crate::ffi`]). Lifecycle state (`ENGINE`) lives here, not in the extern-"C"
//! boundary, so a stray second `engine_start` cannot spawn a competing engine over the
//! RPC socket. Stateless IPC probes stay in `ffi.rs`; only lifecycle is here.
//!
//! Native apps call [`engine_start`] on launch (full engine on a background thread so OS
//! permissions land on the signed app) and [`engine_stop`] on quit. [`engine_start`] also
//! bounded-joins a stale prior thread (see [`join_stale`]) before spawning a replacement
//! while the old one may still be draining (warm-helper kill, HID/hook teardown).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct EngineHandle {
    running: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

static ENGINE: Mutex<Option<EngineHandle>> = Mutex::new(None);

/// Max wait for a stale engine thread before detach (see `join_stale`). Exceeds both
/// platforms' teardown bounds (Windows `shutdown_caps_hook` 2s, macOS
/// `stop_caps_hid_monitor` ~2s) plus warm-helper kill slack. Grow if those grow.
const STALE_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounded wait for a stale (`running == false`) engine thread; detach on timeout
/// (same pattern as `ds_platform::windows::shutdown_caps_hook`). `engine_start` runs on
/// the host UI thread, so an untimed `.join()` could hang: warm-helper kill and platform
/// Drop teardown are not documented as bounded. On timeout, a second engine (and on
/// macOS a second caps-HID monitor) may briefly overlap the stale one — only in this
/// residual wedged-thread case.
fn join_stale(thread: JoinHandle<()>, timeout: Duration) {
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if thread.is_finished() {
            let _ = thread.join();
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    eprintln!(
        "dontspeak: engine_start gave up waiting {timeout:?} for the previous engine \
         thread to finish draining; detaching it instead of blocking the caller"
    );
}

/// Start the engine on a background thread if not already running.
/// Returns true if now running (started or already up), false on spawn failure.
pub(crate) fn engine_start() -> bool {
    // Take the slot under the lock, then release before any join (join can take
    // STALE_JOIN_TIMEOUT). Same lock-drop-before-join pattern as engine_stop.
    let stale = {
        let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(h) if h.running.load(Ordering::SeqCst) => return true, // already running
            _ => slot.take(),
        }
    };
    // Join stale handle first: without this, engine_start while the old thread still
    // drains (IPC Shutdown / relaunch-budget give-up clear `running` early) overlaps two
    // engines, and on macOS two IOHIDManager caps monitors (see iohid's debug_assert_eq!).
    if let Some(mut h) = stale
        && let Some(t) = h.thread.take()
    {
        join_stale(t, STALE_JOIN_TIMEOUT);
    }

    let running = Arc::new(AtomicBool::new(true));
    let reload = Arc::new(AtomicBool::new(false));
    let (r, rl) = (running.clone(), reload.clone());
    // engine_run returns Err instead of process::exit (would kill the host app). Clear
    // `running` on failure so a later start can retry.
    let thread = std::thread::Builder::new()
        .name("ds-engine".into())
        .spawn(move || {
            if let Err(e) = dontspeakd::engine_run(r.clone(), rl) {
                eprintln!("dontspeak: engine startup failed: {e}");
                r.store(false, Ordering::SeqCst);
            }
        })
        .ok();
    if thread.is_none() {
        return false;
    }
    *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(EngineHandle {
        running,
        reload,
        thread,
    });
    true
}

/// Stop the engine (clear run flag, join). True if one was running. Safe on quit.
pub(crate) fn engine_stop() -> bool {
    let handle = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take(); // drop lock before join
    match handle {
        Some(mut h) => {
            h.running.store(false, Ordering::SeqCst);
            if let Some(t) = h.thread.take() {
                join_stale(t, STALE_JOIN_TIMEOUT);
            }
            true
        }
        None => false,
    }
}

/// Ask the running engine to re-read config (no restart). True if an engine is up.
pub(crate) fn engine_reload() -> bool {
    match ENGINE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(h) => {
            h.reload.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Already-finished stale handle must join promptly, not wait out the full timeout.
    #[test]
    fn join_stale_joins_an_already_finished_thread_promptly() {
        let t = std::thread::spawn(|| {});
        while !t.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let start = Instant::now();
        join_stale(t, Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "an already-finished thread must join near-instantly, not wait out the timeout"
        );
    }

    /// Thread still running past timeout must detach (return) rather than hang the UI thread.
    #[test]
    fn join_stale_detaches_a_thread_that_outlives_the_timeout() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            let _ = rx.recv(); // blocks until this test drops `tx` below
        });
        let start = Instant::now();
        join_stale(t, Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50) && elapsed < Duration::from_secs(2),
            "must wait out the timeout, then return promptly without blocking further: {elapsed:?}"
        );
        drop(tx); // unblock the detached thread so it doesn't outlive the test process
    }
}
