//! The engine→UI status push: a dedicated background thread blocks in
//! `ds_model_status_wait` and forwards each new [`ModelStatus`] over an async-channel
//! the GTK main loop drains (mirrors the macOS AsyncStream / Windows push-thread design).

use ds_status::ModelStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One parsed status push. `up == false` means the engine is down (empty `{}` payload).
#[derive(Clone)]
pub struct Snapshot {
    pub up: bool,
    pub status: Option<ModelStatus>,
}

/// Parse a `model_status` JSON string. A non-`ModelStatus` payload (`{}` when the engine is
/// down, or junk) yields a down snapshot rather than an error.
pub fn parse(json: &str) -> Snapshot {
    match serde_json::from_str::<ModelStatus>(json) {
        Ok(s) => Snapshot {
            up: true,
            status: Some(s),
        },
        Err(_) => Snapshot {
            up: false,
            status: None,
        },
    }
}

/// A handle to signal the status thread to stop and wait for it to exit, so
/// `engine_stop()` doesn't race an in-flight `model_status_wait` IPC call.
pub struct StatusThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StatusThread {
    /// Signal the thread to stop and block until it has exited.
    pub fn join(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the push thread. It blocks in `model_status_wait` (1 s guard) and sends a
/// [`Snapshot`] on every change. When the engine is down (immediate `{}`), it throttles so
/// it never busy-spins. Returns a [`StatusThread`] so the caller can signal stop and join
/// before `engine_stop()`, ensuring the thread isn't mid-IPC at shutdown.
pub fn spawn_push(tx: async_channel::Sender<Snapshot>) -> StatusThread {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let handle = std::thread::Builder::new()
        .name("ds-status-push".into())
        .spawn(move || {
            let mut since = 0u64;
            let mut last_up: Option<bool> = None;
            let mut delivered = false;
            loop {
                if tx.is_closed() || stop_t.load(Ordering::Acquire) {
                    break;
                }
                let json = crate::ffi::model_status_wait(since, 1000);
                let snap = parse(&json);
                let unchanged = delivered
                    && snap
                        .status
                        .as_ref()
                        .is_some_and(|status| status.seq == since);
                match &snap.status {
                    Some(s) => since = s.seq,
                    None => since = 0,
                }
                let down = !snap.up;
                let duplicate_down = down && last_up == Some(false);
                last_up = Some(snap.up);
                if !unchanged && !duplicate_down && tx.force_send(snap).is_err() {
                    break; // receiver gone → app closing
                }
                if !unchanged && !duplicate_down {
                    delivered = true;
                }
                if down {
                    // A down engine returns `{}` immediately; don't hammer the wait.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        })
        .ok();
    StatusThread { stop, handle }
}
