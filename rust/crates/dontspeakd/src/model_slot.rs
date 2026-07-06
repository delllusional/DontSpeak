//! `ModelSlot` — a per-model (TTS/STT) residency+error state machine.
//!
//! `tts.rs` used to track each model's state as two independent primitives: an
//! `AtomicBool` "is it loaded" flag and a `Mutex<Option<String>>` "last load error"
//! slot, kept in lockstep by hand at every call site (`mark_loaded` + a paired
//! `clear_*_load_error`). Collapsing them into one enum removes the "kept in
//! lockstep by convention, not by the type system" risk — a single write now
//! replaces what used to be two.

use crate::status::StatusGate;
use std::sync::Mutex;

/// One model's (TTS or STT) residency state, as reported by the warm helper's own
/// confirmation lines (`TTSLOADED`/`STTLOADED`/`TTSLOADERR`/`STTLOADERR`) or an
/// explicit unload/teardown (`Idle`). No `Loading` variant: nothing reads a
/// distinct "in flight" state from here — "warming" is derived elsewhere
/// (`status.rs::engine_state()`) from config + on-disk file presence, not from
/// this tracker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelState {
    Idle,
    Loaded,
    Failed(String),
}

/// A [`ModelState`] behind a `Mutex`, with change-gated transitions so a
/// `StatusGate` bump only fires on a REAL change. This matters because two things
/// this channel exists for both like to repeat identically: the periodic self-heal
/// reconcile (`config_gate::reconcile_helper_models`) can re-report an
/// already-loaded model's `TTSLOADED`/`STTLOADED` every tick, and a transient
/// AV-scan-retry failure can repeat the SAME message several times in a row —
/// neither must spam a blocked `WaitModelStatus` waiter.
pub(crate) struct ModelSlot(Mutex<ModelState>);

impl ModelSlot {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(ModelState::Idle))
    }

    /// Change-gated transition to `new_state` — bumps `gate` iff this is a REAL
    /// change from the current state. Reserved for the AUTHORITATIVE signal: the
    /// helper's own `TTSLOADED`/`STTLOADED`/`TTSLOADERR`/`STTLOADERR` confirmation
    /// lines, and explicit unload/teardown paths (transitioning to `Idle`).
    pub(crate) fn transition(&self, new_state: ModelState, gate: Option<&StatusGate>) {
        let mut guard = self.0.lock().unwrap();
        if *guard != new_state {
            *guard = new_state;
            drop(guard);
            if let Some(g) = gate {
                g.bump();
            }
        }
    }

    /// `Failed -> Idle` only; a no-op if already `Idle` or `Loaded` — a parallel
    /// preload that already confirmed `Loaded` earlier in the same call must not be
    /// regressed by a later "clear any stale error from a prior child" pass.
    pub(crate) fn clear_error(&self, gate: Option<&StatusGate>) {
        let mut guard = self.0.lock().unwrap();
        if matches!(*guard, ModelState::Failed(_)) {
            *guard = ModelState::Idle;
            drop(guard);
            if let Some(g) = gate {
                g.bump();
            }
        }
    }

    /// `Idle -> Loaded` ONLY, WITHOUT bumping the gate — for `play()`/`listen()`'s
    /// per-request optimistic path. Takes NO gate parameter at all, by construction,
    /// so it can never be wired to bump even by a future mistake: `play()`/`listen()`
    /// run on EVERY request, so wiring a gate bump into that hot path would
    /// reintroduce request-rate status churn. The authoritative "just became
    /// resident" push stays the helper's own `TTSLOADED`/`STTLOADED` confirmation via
    /// [`transition`](Self::transition) — this store only exists so
    /// `is_tts_loaded`/`is_stt_loaded` don't read stale-false in the gap before that
    /// confirmation lands.
    ///
    /// Deliberately leaves `Failed(_)` (and `Loaded`) alone rather than papering over
    /// them: a model that's genuinely `Failed` and hasn't yet had a fresh load
    /// confirmed by the helper must keep showing its error, not flip green just
    /// because a request happened to come in — e.g. `stt_test.rs`'s Settings "Test
    /// Recognition" path calls `listen()` gated only on file presence, not
    /// `is_stt_loaded()`, so it can reach here while STT is genuinely `Failed`; if
    /// this cleared the error, that would silently erase the one visible symptom of
    /// that gate bypass. A real recovery still clears the error normally, just via
    /// the authoritative path (`transition(Loaded, gate)` on the helper's own
    /// `TTSLOADED`/`STTLOADED`), which unconditionally overwrites any prior state
    /// including `Failed`.
    pub(crate) fn mark_loaded_optimistic(&self) {
        let mut guard = self.0.lock().unwrap();
        if *guard == ModelState::Idle {
            *guard = ModelState::Loaded;
        }
    }

    pub(crate) fn is_loaded(&self) -> bool {
        matches!(*self.0.lock().unwrap(), ModelState::Loaded)
    }

    pub(crate) fn error(&self) -> Option<String> {
        match &*self.0.lock().unwrap() {
            ModelState::Failed(msg) => Some(msg.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_transition_bumps() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.transition(ModelState::Loaded, Some(&gate));
        assert_ne!(gate.seq(), 0, "the first transition bumps the gate");
        assert!(slot.is_loaded());
    }

    #[test]
    fn identical_repeat_does_not_bump() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.transition(ModelState::Loaded, Some(&gate));
        let seq1 = gate.seq();
        slot.transition(ModelState::Loaded, Some(&gate));
        assert_eq!(gate.seq(), seq1, "an identical repeat must not bump again");
    }

    #[test]
    fn a_different_failed_message_bumps_again() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.transition(ModelState::Failed("boom".into()), Some(&gate));
        let seq1 = gate.seq();
        assert_ne!(seq1, 0);

        slot.transition(ModelState::Failed("boom".into()), Some(&gate));
        assert_eq!(
            gate.seq(),
            seq1,
            "an identical failure message must not bump again"
        );

        slot.transition(ModelState::Failed("different".into()), Some(&gate));
        assert_ne!(gate.seq(), seq1, "a DIFFERENT message bumps again");
        assert_eq!(slot.error().as_deref(), Some("different"));
    }

    #[test]
    fn clear_error_on_loaded_is_a_noop() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.transition(ModelState::Loaded, Some(&gate));
        let seq1 = gate.seq();
        slot.clear_error(Some(&gate));
        assert_eq!(gate.seq(), seq1, "clear_error on Loaded must not touch it");
        assert!(slot.is_loaded());
    }

    #[test]
    fn clear_error_on_idle_is_a_noop() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.clear_error(Some(&gate));
        assert_eq!(
            gate.seq(),
            0,
            "clear_error on a never-touched Idle must not bump"
        );
    }

    #[test]
    fn mark_loaded_optimistic_never_bumps_regardless_of_prior_state() {
        let gate = StatusGate::new();

        let idle_slot = ModelSlot::new();
        idle_slot.mark_loaded_optimistic();
        assert_eq!(gate.seq(), 0);
        assert!(idle_slot.is_loaded());

        // From `Failed`: no bump, and (per `mark_loaded_optimistic_leaves_a_failed_state_alone`
        // below) no transition either — covered again here to also assert the gate side.
        let failed_slot = ModelSlot::new();
        failed_slot.transition(ModelState::Failed("boom".into()), Some(&gate));
        let seq_after_fail = gate.seq();
        failed_slot.mark_loaded_optimistic();
        assert_eq!(
            gate.seq(),
            seq_after_fail,
            "mark_loaded_optimistic must never bump the gate"
        );
        assert!(!failed_slot.is_loaded());
    }

    #[test]
    fn mark_loaded_optimistic_leaves_a_failed_state_alone() {
        // The Test-Recognition regression guard: a genuinely `Failed` model must keep
        // showing its error across an optimistic `play()`/`listen()` write, not flip green
        // just because a request happened to come in while unresolved.
        let slot = ModelSlot::new();
        slot.transition(ModelState::Failed("boom".into()), None);

        slot.mark_loaded_optimistic();

        assert!(
            !slot.is_loaded(),
            "mark_loaded_optimistic must not paper over a Failed state"
        );
        assert_eq!(
            slot.error().as_deref(),
            Some("boom"),
            "the error message must survive unchanged"
        );
    }

    #[test]
    fn mark_loaded_optimistic_is_a_noop_from_loaded() {
        let slot = ModelSlot::new();
        slot.transition(ModelState::Loaded, None);
        slot.mark_loaded_optimistic();
        assert!(slot.is_loaded());
    }
}
