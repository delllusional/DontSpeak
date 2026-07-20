//! Per-model (TTS/STT) residency + error as one enum (was loaded-bool + error
//! mutex kept in lockstep by convention at every call site).

use crate::status::StatusGate;
use std::sync::Mutex;

/// Helper-reported residency (TTSLOADED/…/unload). No Loading — warming is derived in status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelState {
    Idle,
    Loaded,
    Failed(String),
}

/// Change-gated [`ModelState`]: only real changes bump WaitModelStatus (reconcile/AV-retry spam).
pub(crate) struct ModelSlot(Mutex<ModelState>);

impl ModelSlot {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(ModelState::Idle))
    }

    /// Authoritative transition; bump gate — and return `true` — only on real change,
    /// so callers can also change-gate their own side effects (e.g. logging).
    pub(crate) fn transition(&self, new_state: ModelState, gate: Option<&StatusGate>) -> bool {
        let mut guard = self.0.lock().unwrap();
        if *guard != new_state {
            *guard = new_state;
            drop(guard);
            if let Some(g) = gate {
                g.bump();
            }
            return true;
        }
        false
    }

    /// `Failed → Idle` only (never regress Loaded).
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

    /// `Idle → Loaded` only, no gate bump (hot path). Leaves Failed alone so Test
    /// Recognition can't greenwash a real error; recovery is via [`transition`].
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
        assert!(
            slot.transition(ModelState::Loaded, Some(&gate)),
            "a real change reports true"
        );
        assert_ne!(gate.seq(), 0, "the first transition bumps the gate");
        assert!(slot.is_loaded());
    }

    #[test]
    fn identical_repeat_does_not_bump() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();
        slot.transition(ModelState::Loaded, Some(&gate));
        let seq1 = gate.seq();
        assert!(
            !slot.transition(ModelState::Loaded, Some(&gate)),
            "an identical repeat reports false"
        );
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
    fn clear_error_is_a_noop_outside_failed() {
        // Loaded: already clear; Idle: never had an error — neither must bump the gate.
        let loaded = ModelSlot::new();
        let gate = StatusGate::new();
        loaded.transition(ModelState::Loaded, Some(&gate));
        let seq1 = gate.seq();
        loaded.clear_error(Some(&gate));
        assert_eq!(gate.seq(), seq1, "clear_error on Loaded must not touch it");
        assert!(loaded.is_loaded());

        let idle = ModelSlot::new();
        let gate = StatusGate::new();
        idle.clear_error(Some(&gate));
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

        // Already Loaded: re-mark is a pure noop (stays loaded, no gate side-effect).
        let loaded_slot = ModelSlot::new();
        loaded_slot.transition(ModelState::Loaded, None);
        loaded_slot.mark_loaded_optimistic();
        assert!(loaded_slot.is_loaded());
        assert_eq!(gate.seq(), 0);

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
}
