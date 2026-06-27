//! The engine→UI status push: a dedicated background thread blocks in
//! `ds_model_status_wait` and forwards each new [`ModelStatus`] over an async-channel
//! the GTK main loop drains (mirrors the macOS AsyncStream / Windows push-thread design).

use ds_status::ModelStatus;

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

/// Spawn the push thread. It blocks in `model_status_wait` (1 s guard) and sends a
/// [`Snapshot`] on every change. When the engine is down (immediate `{}`), it throttles so
/// it never busy-spins. Ends when the receiver is dropped (the app is closing).
pub fn spawn_push(tx: async_channel::Sender<Snapshot>) {
    std::thread::Builder::new()
        .name("ds-status-push".into())
        .spawn(move || {
            let mut since = 0u64;
            loop {
                let json = crate::ffi::model_status_wait(since, 1000);
                let snap = parse(&json);
                match &snap.status {
                    Some(s) => since = s.seq,
                    None => since = 0,
                }
                let down = !snap.up;
                if tx.send_blocking(snap).is_err() {
                    break; // receiver gone → app closing
                }
                if down {
                    // A down engine returns `{}` immediately; don't hammer the wait.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        })
        .ok();
}
