//! Logs→UI push, mirroring `status::spawn_push` in shape but SCOPED to the Log tab's
//! visibility (started/stopped by `ui.rs`'s tab-visibility handler) rather than the whole app
//! lifetime, since polling logs while the tab is closed is pure waste.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Spawn the push thread: blocks in `ffi::log_wait` (2s guard) and sends the flattened tail on
/// every wake. Returns the stop flag — `store(true, Relaxed)` to end it; the thread notices
/// within its own wait timeout (or immediately, once its current wait returns) and exits.
pub fn spawn_push(tx: async_channel::Sender<String>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("ds-logs-push".into())
        .spawn(move || {
            // Prime on this worker before waiting for the next change. The old caller-side
            // prime performed file I/O synchronously in the tab-selection callback.
            let initial = crate::ffi::log_tail(64 * 1024);
            if stop2.load(Ordering::Relaxed) || tx.send_blocking(initial).is_err() {
                return;
            }
            while !stop2.load(Ordering::Relaxed) {
                let text = crate::ffi::log_wait(64 * 1024, 2000);
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                if tx.send_blocking(text).is_err() {
                    break; // receiver gone — the tab was closed and torn down
                }
            }
        })
        .ok();
    stop
}
