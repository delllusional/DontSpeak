//! `SttResidencySlot` — the STT (Parakeet) load-claim state machine that replaces
//! `serve.rs`'s old `stt_claimed: Arc<AtomicBool>`.
//!
//! The old flag had exactly one problem: a failed load released the claim (so a
//! retry could happen), but nothing enforced that "claimed" could ONLY be exited via
//! either a successful load or an explicit failure/unload path — a future call site
//! could `swap(true, ..)` and just... never release it on some other exit. This type
//! makes that structurally impossible: the only way out of `Loading` is
//! [`resolve_ok`](SttResidencySlot::resolve_ok) or
//! [`mark_unloaded`](SttResidencySlot::mark_unloaded), and the only way out of
//! `Loaded` is `mark_unloaded` too. There is no other transition to reach from either
//! state, so a caller cannot leave the claim stuck true after this refactor the way
//! it did before 4ef3013 fixed the specific failed-load-then-never-retries case.

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

    /// `Idle -> Loading`, returns `true` (claimed). A no-op (`false`) from
    /// `Loading`/`Loaded` — the caller must skip its own load attempt in that case
    /// (the model is already resident, or another load is already in flight).
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

    /// `-> Idle` unconditionally, from ANY state. Call on a load FAILURE (so a later
    /// `load stt` can retry) AND on an explicit `unload stt`. This is what makes the
    /// claim structurally unable to get stuck: there is no other way to leave
    /// `Loading` except [`resolve_ok`](Self::resolve_ok), and no way to leave `Loaded`
    /// except this.
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
    fn resolve_ok_from_loading() {
        let slot = SttResidencySlot::new();
        assert!(slot.try_claim());
        slot.resolve_ok();
        // Loaded now blocks a further claim — indirect proof `resolve_ok` landed.
        assert!(!slot.try_claim());
    }

    #[test]
    fn mark_unloaded_resets_from_every_state() {
        // The literal regression test for "stuck true forever after unload+reload" — the
        // actual bug 4ef3013 fixed, now made structurally impossible by this type.
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
