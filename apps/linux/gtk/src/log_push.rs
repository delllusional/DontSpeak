//! Logs→UI push while the Log tab is open (like `status::spawn_push`).
//! Raw `ds_logs_json` so the UI can filter via [`ds_log`] without another disk read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Blocks in `log_wait_json` (2s); sends raw JSON. Stop: `store(true, Relaxed)`.
pub fn spawn_push(tx: async_channel::Sender<String>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("ds-logs-push".into())
        .spawn(move || {
            // Prime on this worker — avoid file I/O in the tab-selection callback.
            let initial = crate::ffi::log_tail_json(64 * 1024);
            if stop2.load(Ordering::Relaxed) || tx.send_blocking(initial).is_err() {
                return;
            }
            while !stop2.load(Ordering::Relaxed) {
                let text = crate::ffi::log_wait_json(64 * 1024, 2000);
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
