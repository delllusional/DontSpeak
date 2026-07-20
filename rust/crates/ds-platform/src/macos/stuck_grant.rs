//! Shared "denied while already trusted" latch for `IOHIDManagerOpen` (see `iohid.rs`).
//!
//! Mid-process Accessibility grant does not unstick open attempts in-process — only
//! relaunch does. Distinguishes "waiting for grant" from "granted but stuck" (engine
//! self-relaunch). Shared by caps-HID monitor and LED writer; each keeps its own retry
//! loop, only the denial-while-trusted count lives here.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use super::iokit;

/// How long a retry loop waits between `IOHIDManagerOpen` attempts while denied.
/// Shared by `iohid.rs`'s dedicated monitor thread and `led.rs`'s throttled retry —
/// both hit the identical underlying permission, so this is one tuning decision,
/// not two independent constants that could drift.
pub(crate) const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Consecutive denials tolerated WHILE Accessibility is already trusted before
/// concluding a resource's grant is stale and latching [`StuckGrantLatch::is_stuck`]
/// — see the module doc's bug-class summary. ~3 ticks (~6s at [`RETRY_INTERVAL`])
/// absorbs a race where AX flips true a moment before a given check without making
/// the user wait minutes for the automatic relaunch. Shared by both `iohid.rs` and
/// `led.rs`'s latches for the same reason `RETRY_INTERVAL` is.
pub(crate) const STUCK_RETRIES_BEFORE_RELAUNCH: u32 = 3;

/// Counts consecutive denials seen WHILE `AXIsProcessTrusted()` is already true;
/// [`is_stuck`](Self::is_stuck) is just that count compared to a threshold — no
/// separate latch flag to keep in sync, since the count only ever moves by exactly
/// 1 per call and is reset (to 0) on success or an untrusted read, so "crossed the
/// threshold" and "still at/above it" fall out of the one number. Meant to be a
/// `static` at each call site (not a field on a per-instance struct): `MacOsPlatform`
/// is reconstructed fresh whenever `ds_engine_start()` (`ds-core::host`) runs again in
/// the SAME OS process after the previous engine thread has already exited on its own
/// — which happens after an IPC `Request::Shutdown` (`dontspeakd::ipc`) or after
/// `dontspeakd::boot::engine_run`'s relaunch-budget-exhausted give-up path returns.
/// Neither a config reload nor any RPC-driven "restart" reconstructs it: a config
/// reload (`Engine::reload`) reuses the existing `MacOsPlatform` in place, and no
/// RPC-driven restart request exists in the wire protocol at all (`ds_ipc::Request` has
/// only `Shutdown`). A per-instance counter would reset on each of those legitimate
/// in-process restarts and could let denials spread across two of them never cross the
/// threshold, silently defeating the detector.
pub(crate) struct StuckGrantLatch {
    denied_while_trusted: AtomicU32,
    threshold: u32,
}

impl StuckGrantLatch {
    pub(crate) const fn new(threshold: u32) -> Self {
        Self {
            denied_while_trusted: AtomicU32::new(0),
            threshold,
        }
    }

    /// Record one denied attempt. Resets the streak while Accessibility still reads
    /// untrusted (the ordinary "waiting for the user" wait, which must stay
    /// unbounded); accumulates it once trusted. Returns `Some(count)` on the EXACT
    /// call that first reaches `threshold` — so the caller can log once — and `None`
    /// on every other call (still waiting, not yet at the threshold, or already past
    /// it).
    pub(crate) fn record_denial(&self) -> Option<u32> {
        if !iokit::ax_is_process_trusted() {
            self.denied_while_trusted.store(0, Ordering::Relaxed);
            return None;
        }
        let count = self.denied_while_trusted.fetch_add(1, Ordering::Relaxed) + 1;
        (count == self.threshold).then_some(count)
    }

    /// Record a successful open: clears the streak, since this attempt just proved
    /// the process's grant is NOT permanently stale after all (it may have
    /// recovered on its own a beat after latching, or the engine hadn't yet acted on
    /// it) — letting a stale stuck count linger would risk an unnecessary
    /// self-relaunch later even though this resource is fine now.
    pub(crate) fn record_success(&self) {
        self.denied_while_trusted.store(0, Ordering::Relaxed);
    }

    pub(crate) fn is_stuck(&self) -> bool {
        self.denied_while_trusted.load(Ordering::Relaxed) >= self.threshold
    }
}

/// The one place the "stuck, asking the engine to relaunch" message is worded, so
/// `iohid.rs` and `led.rs` can't drift into two subtly different phrasings of the
/// same event. `resource` names what was denied (e.g. "caps HOLD", "Caps LED
/// writer") for the one call-site-specific word in an otherwise identical message.
pub(crate) fn log_stuck(resource: &str, count: u32) {
    log::warn!(
        target: "platform",
        "{resource} stuck: Accessibility is trusted but IOHIDManagerOpen \
         has denied {count} times anyway — this process's grant is stale and won't \
         self-heal in place; asking the engine to relaunch"
    );
}
