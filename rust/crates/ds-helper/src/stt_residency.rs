//! `SttResidencySlot` — STT load-claim state machine replacing `serve.rs`'s old
//! `stt_claimed: Arc<AtomicBool>`.
//!
//! The bool could release on failed load (retry OK) but nothing forced "claimed"
//! to exit only via success or explicit failure/unload — a call site could
//! `swap(true)` and never release. Here the only exits from `Loading` are
//! [`resolve_ok`](SttResidencySlot::resolve_ok) / [`mark_unloaded`](SttResidencySlot::mark_unloaded),
//! and from `Loaded` only `mark_unloaded` — so the claim can't stick true the way
//! it did before 4ef3013 (failed load then never retries).

use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SttResidency {
    Idle,
    Loading,
    Loaded,
}

pub(crate) struct SttResidencySlot(Mutex<SttResidency>);

impl SttResidencySlot {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(SttResidency::Idle))
    }

    /// `Idle -> Loading` (`true`). No-op (`false`) from `Loading`/`Loaded` — caller must skip load.
    pub(crate) fn try_claim(&self) -> bool {
        let mut guard = self.0.lock().unwrap();
        if *guard == SttResidency::Idle {
            *guard = SttResidency::Loading;
            true
        } else {
            false
        }
    }

    /// `Loading -> Loaded`. Call at the same point `STTLOADED` is printed.
    pub(crate) fn resolve_ok(&self) {
        *self.0.lock().unwrap() = SttResidency::Loaded;
    }

    /// `Loaded` residency — distinguishes the steady state from an in-flight `Loading` claim.
    pub(crate) fn is_loaded(&self) -> bool {
        *self.0.lock().unwrap() == SttResidency::Loaded
    }

    /// `-> Idle` from any state. On load failure (so later `load stt` can retry) and on
    /// `unload stt`. Structural unstick: only exit from `Loading`/`Loaded` (with `resolve_ok`).
    pub(crate) fn mark_unloaded(&self) {
        *self.0.lock().unwrap() = SttResidency::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_claim_succeeds_from_idle() {
        let slot = SttResidencySlot::new();
        assert!(slot.try_claim());
    }

    #[test]
    fn try_claim_fails_from_loading() {
        let slot = SttResidencySlot::new();
        assert!(slot.try_claim());
        assert!(!slot.try_claim(), "a second claim while Loading must fail");
    }

    #[test]
    fn try_claim_fails_from_loaded() {
        let slot = SttResidencySlot::new();
        assert!(slot.try_claim());
        slot.resolve_ok();
        assert!(!slot.try_claim(), "a claim while already Loaded must fail");
    }

    #[test]
    fn is_loaded_only_after_resolve_ok() {
        // The serve loop stays silent on Loaded skips (steady-state reconcile tick)
        // but still logs an in-flight (Loading) collision — the distinction is this flag.
        let slot = SttResidencySlot::new();
        assert!(!slot.is_loaded());
        assert!(slot.try_claim());
        assert!(!slot.is_loaded(), "Loading is not Loaded");
        slot.resolve_ok();
        assert!(slot.is_loaded());
        slot.mark_unloaded();
        assert!(!slot.is_loaded());
    }

    #[test]
    fn resolve_ok_from_loading() {
        let slot = SttResidencySlot::new();
        assert!(slot.try_claim());
        slot.resolve_ok();
        // Loaded blocks further claim — proof resolve_ok landed.
        assert!(!slot.try_claim());
    }

    #[test]
    fn mark_unloaded_resets_from_every_state() {
        // Regression: "stuck true forever after unload+reload" (4ef3013), now structural.
        let idle = SttResidencySlot::new();
        idle.mark_unloaded();
        assert!(
            idle.try_claim(),
            "mark_unloaded from Idle must stay claimable"
        );

        let from_loading = SttResidencySlot::new();
        assert!(from_loading.try_claim());
        from_loading.mark_unloaded();
        assert!(
            from_loading.try_claim(),
            "mark_unloaded from Loading must release the claim"
        );

        let from_loaded = SttResidencySlot::new();
        assert!(from_loaded.try_claim());
        from_loaded.resolve_ok();
        from_loaded.mark_unloaded();
        assert!(
            from_loaded.try_claim(),
            "mark_unloaded from Loaded must release the claim"
        );
    }
}
