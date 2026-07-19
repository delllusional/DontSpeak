//! Engine→UI status push: background thread blocks in `ds_model_status_wait` and forwards
//! each [`ModelStatus`] over an async-channel the GTK main loop drains (macOS AsyncStream /
//! Windows push-thread analogue).

use ds_status::ModelStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One parsed status push. `up == false` means the engine is down (empty `{}` payload).
#[derive(Clone)]
pub struct Snapshot {
    pub up: bool,
    pub status: Option<ModelStatus>,
}

/// Parse `model_status` JSON. Non-`ModelStatus` payload (`{}` when down, or junk) → down snapshot.
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

/// Stop + join so `engine_stop()` doesn't race an in-flight `model_status_wait`.
pub struct StatusThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StatusThread {
    pub fn join(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the push thread (`model_status_wait`, 1 s guard). Throttles when the engine is down
/// (immediate `{}`) with a 500 ms sleep. Join via [`StatusThread`] before `engine_stop()`.
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
                    // Down engine returns `{}` immediately; 500 ms throttle (see spawn_push).
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        })
        .ok();
    StatusThread { stop, handle }
}
