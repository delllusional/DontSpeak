//! The in-process engine host — the ONE owner of the background engine thread, used by the
//! C ABI ([`crate::ffi`]). The lifecycle state (the `ENGINE` static) lives here, OUT of the
//! extern-"C" boundary: keeping the spawn/join + run flag in one place means a stray second
//! `engine_start` can't spin up a competing engine that would fight over the RPC socket. The
//! stateless IPC probes (status/mute/provider) hold no mutable state, so `ffi.rs` calls the
//! IPC directly; only the lifecycle lives here.
//!
//! A native app calls [`engine_start`] on launch to run the FULL engine — caps loop, RPC
//! server, TTS queue, hot-reload — on a background thread INSIDE the app process, so the
//! OS permissions land on the one signed app. [`engine_stop`] (on quit) clears the run
//! flag and joins the thread.
//!
//! [`engine_start`] also joins (bounded — see [`join_stale`]) a stale prior thread whose
//! `running` flag already went false but which is still draining (warm-helper kill+wait,
//! platform HID/hook teardown) before spawning a replacement, so a stray second call can no
//! longer spin up a competing engine over one still shutting down.

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

/// How long `engine_start` waits for a stale engine thread to finish draining before
/// giving up and detaching it (see `join_stale`). Comfortably exceeds both platforms'
/// own known/expected teardown bounds — Windows' hard 2s `shutdown_caps_hook` timeout
/// (`ds_platform::windows`) and macOS's ~2s expected `stop_caps_hid_monitor` latency
/// (`ds_platform::macos::iohid`) — plus slack for the warm-helper kill+wait and the rest
/// of `boot::engine_run`'s shutdown sequence, so an ordinary — if slow — shutdown never
/// trips it. If either platform's own inner bound ever grows, this must grow with it.
const STALE_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounded wait for a stale (`running == false`) engine thread to actually finish,
/// mirroring `ds_platform::windows::shutdown_caps_hook`'s poll-with-timeout,
/// detach-on-timeout strategy for its own WH_KEYBOARD_LL pump-thread join (identical
/// freeze risk, identical fix — see that function's doc). `engine_start` runs
/// SYNCHRONOUSLY on the host app's main/UI thread on all three platforms (macOS
/// `DontSpeakApp.swift`, Windows `App.xaml.cs`, Linux `main.rs`), so an untimed
/// `.join()` here would risk hanging that thread: neither the warm-helper kill+wait
/// (`dontspeakd::tts::TtsManager::stop_child`) nor either platform's own resource
/// teardown (`WindowsPlatform`/`MacOsPlatform`'s `Drop`, the last thing
/// `dontspeakd::boot::engine_run` does before returning) is documented as bounded. On
/// timeout this logs and DETACHES (drops the `JoinHandle` without joining) rather than
/// blocking forever — a second engine thread, and on macOS a second caps-HID monitor,
/// may then run briefly alongside the stale one until it actually finishes, exactly the
/// pre-existing behavior for every call before this fix, but now only in this residual
/// wedged-thread case instead of on every call.
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

/// Start the engine on a background thread if not already running. Returns true if it is
/// now running (started or already up), false on spawn failure.
pub(crate) fn engine_start() -> bool {
    // Check "already running" and — if not — take the slot's contents (a stale handle,
    // or None) atomically under the lock, so a concurrent engine_stop/engine_reload
    // never observes a half-updated slot. The lock is released again immediately after
    // (see below) and must NEVER be held across the join below, which — unlike this
    // fast in-memory check — can take up to STALE_JOIN_TIMEOUT. Mirrors engine_stop,
    // which also drops the lock before its own join (see its comment).
    let stale = {
        let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(h) if h.running.load(Ordering::SeqCst) => return true, // already running
            _ => slot.take(),
        }
    };
    // Join any stale (non-running) handle BEFORE spawning a replacement — see the
    // module doc and join_stale's doc for why: without this, a second engine_start
    // landing while the OLD engine thread is still draining (an IPC Request::Shutdown,
    // or boot::engine_run's relaunch-budget-exhausted give-up path — both clear
    // `running` well before the thread actually returns) would overlap two live
    // engines, and on macOS two live IOHIDManager caps monitors (see
    // ds_platform::macos::iohid's debug_assert_eq! tripwire for exactly this).
    if let Some(mut h) = stale
        && let Some(t) = h.thread.take()
    {
        join_stale(t, STALE_JOIN_TIMEOUT);
    }

    let running = Arc::new(AtomicBool::new(true));
    let reload = Arc::new(AtomicBool::new(false));
    let (r, rl) = (running.clone(), reload.clone());
    // engine_run RETURNS a fatal startup error instead of process::exit()ing — which here,
    // on a background thread INSIDE the host app, would have killed the whole app. On Err,
    // log it and CLEAR the running flag so a subsequent start can retry rather than wedge
    // "running".
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

/// Stop the engine (clear the run flag, join the thread). Returns true if an engine was
/// running, false if none. Safe to call on quit.
pub(crate) fn engine_stop() -> bool {
    let handle = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take(); // drop the lock before joining
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

/// Ask the running engine to re-read its config (no restart). Returns true if an engine
/// is running, else false.
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

    /// The common case this fix targets: a stale handle that has ALREADY finished by the
    /// time join_stale runs must be joined outright, well within its timeout — not wait
    /// out the full bound.
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

    /// The residual case (b)/(c) accept: a stale handle still running past the timeout
    /// must be DETACHED (the call returns) rather than blocking the caller indefinitely —
    /// the main-thread-hang risk this design explicitly trades off.
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
