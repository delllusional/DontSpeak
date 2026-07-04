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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

struct EngineHandle {
    running: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

static ENGINE: Mutex<Option<EngineHandle>> = Mutex::new(None);

/// Start the engine on a background thread if not already running. Returns true if it is
/// now running (started or already up), false on spawn failure.
pub(crate) fn engine_start() -> bool {
    let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if slot
        .as_ref()
        .is_some_and(|h| h.running.load(Ordering::SeqCst))
    {
        return true; // already running
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
    *slot = Some(EngineHandle {
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
                let _ = t.join();
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
