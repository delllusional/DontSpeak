//! Logs→UI push, shaped like `status::spawn_push` but scoped to Log-tab visibility
//! (started/stopped from `ui.rs`) — polling while the tab is closed is pure waste.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Spawn the push thread: blocks in `ffi::log_wait` (2s guard) and sends the flattened tail
/// on every wake. Returns the stop flag (`store(true, Relaxed)`); thread exits within one wait.
pub fn spawn_push(tx: async_channel::Sender<String>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("ds-logs-push".into())
        .spawn(move || {
            // Prime on this worker — avoid file I/O in the tab-selection callback.
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
                    break; // receiver gone — tab torn down
                }
            }
        })
        .ok();
    stop
}
