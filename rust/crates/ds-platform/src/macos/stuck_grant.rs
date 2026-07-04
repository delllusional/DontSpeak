//! Shared "denied-while-already-trusted" stuck-grant counter/latch.
//!
//! The bug class this exists for (see `iohid.rs`'s module doc for the full story): a
//! macOS Accessibility grant made WHILE the process is already running does not
//! retroactively unstick an already-open (or already-attempted) `IOHIDManagerOpen`
//! handle in that SAME process — recreating the manager doesn't help either. Only a
//! full quit + relaunch clears it. Any code that opens such a handle needs a way to
//! tell "still waiting for the user to grant Accessibility" (normal, unbounded, silent
//! wait) apart from "the grant landed, but THIS process's handle is stuck denied
//! anyway" (needs the app to relaunch itself — see `dontspeakd::boot::engine_run`).
//!
//! Both `iohid.rs`'s caps-HID monitor and `led.rs`'s LED writer hit exactly this
//! shape, so the counter/latch lives here once instead of twice. Each caller keeps its
//! own retry MECHANISM (a dedicated thread blocked in a run loop for the HID monitor,
//! which needs to stay open continuously to receive callbacks; a throttled retry on
//! next use for the LED writer, which only needs to be open at the moment it's
//! actually driven) — only the "how many denials while trusted, and when do we give
//! up and latch" bookkeeping is shared.

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
/// `static` at each call site (not a field on a per-instance struct): the platform
/// object that owns the resource (`MacOsPlatform`) is reconstructed on every
/// in-process engine restart (config reload, RPC-driven restart) WITHOUT the OS
/// process exiting — a per-instance counter would reset on each of those and could
/// let denials spread across two restarts never cross the threshold, silently
/// defeating the detector.
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
    eprintln!(
        "[dontspeak] {resource} stuck: Accessibility is trusted but IOHIDManagerOpen \
         has denied {count} times anyway — this process's grant is stale and won't \
         self-heal in place; asking the engine to relaunch"
    );
}
