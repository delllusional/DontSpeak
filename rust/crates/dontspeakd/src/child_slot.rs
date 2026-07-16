//! Warm `ds-helper` process-lifecycle slot: handle + generation + deliberate-EOF
//! marker as one type (was three hand-synced primitives).
//!
//! Couplings made structural: generation bumps with handle under one mutex;
//! [`install`] always clears `expected_eof`; [`reap`] re-asserts it before take
//! so a child never leaves without a deliberate EOF mark.

use std::process::Child;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Handle + generation + deliberate-EOF (see module doc). Cell mutex is brief only —
/// never across kill/wait; outer `lifecycle` serializes transitions.
pub(crate) struct ChildSlot {
    /// Handle + generation under one mutex (install is one atomic write).
    cell: Mutex<ChildCell>,
    /// Atomic so reader classifies EOF lock-free (never blocks on kill/wait).
    expected_eof: AtomicBool,
}

struct ChildCell {
    child: Option<Child>,
    /// Bumped by [`install`] (`gen` is reserved in edition 2024).
    generation: u64,
}

impl ChildSlot {
    pub(crate) fn new() -> Self {
        Self {
            cell: Mutex::new(ChildCell {
                child: None,
                generation: 0,
            }),
            expected_eof: AtomicBool::new(false),
        }
    }

    /// READY child: handle + gen bump + clear expected-EOF. Under `lifecycle`; no reader yet.
    pub(crate) fn install(&self, child: Child) {
        {
            let mut cell = self.cell.lock().unwrap();
            cell.child = Some(child);
            cell.generation += 1;
        }
        self.expected_eof.store(false, Ordering::Release);
    }

    /// Mark upcoming teardown deliberate (before stdin drop can exit the child).
    pub(crate) fn begin_deliberate_stop(&self) {
        self.expected_eof.store(true, Ordering::Release);
    }

    /// Take child for kill/wait outside locks; re-asserts deliberate EOF.
    pub(crate) fn reap(&self) -> Option<Child> {
        self.expected_eof.store(true, Ordering::Release);
        self.cell.lock().unwrap().child.take()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.cell.lock().unwrap().child.is_some()
    }

    /// Generation of running child, or None if empty.
    pub(crate) fn running_gen(&self) -> Option<u64> {
        let cell = self.cell.lock().unwrap();
        cell.child.is_some().then_some(cell.generation)
    }

    /// Generation even if empty (`mark_dead_if_current` staleness).
    pub(crate) fn generation(&self) -> u64 {
        self.cell.lock().unwrap().generation
    }

    /// `(present, exited)` peek for heal decisions; never takes/kills.
    pub(crate) fn probe(&self) -> (bool, bool) {
        match self.cell.lock().unwrap().child.as_mut() {
            Some(c) => (true, !matches!(c.try_wait(), Ok(None))),
            None => (false, false),
        }
    }

    /// Deliberate EOF? Lock-free for reader classification.
    pub(crate) fn eof_was_expected(&self) -> bool {
        self.expected_eof.load(Ordering::Acquire)
    }

    /// Peek exit status without taking the child.
    pub(crate) fn peek_exit_status(&self) -> Option<std::process::ExitStatus> {
        self.cell
            .lock()
            .unwrap()
            .child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
    }
}

impl Drop for ChildSlot {
    fn drop(&mut self) {
        self.expected_eof.store(true, Ordering::Release);
        let cell = self.cell.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = cell.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A trivial real process for tests that need a live `Child` handle — hermetic
    /// (not the real ds-helper, no I/O): Windows `cmd /C exit 0`, Unix `true`.
    fn dummy_child() -> Child {
        #[cfg(windows)]
        let spawned = Command::new("cmd").args(["/C", "exit 0"]).spawn();
        #[cfg(not(windows))]
        let spawned = Command::new("true").spawn();
        spawned.expect("spawn dummy child")
    }

    #[test]
    fn new_slot_is_empty_gen_zero_eof_unexpected() {
        let slot = ChildSlot::new();
        assert!(!slot.is_running());
        assert_eq!(slot.generation(), 0);
        assert_eq!(slot.running_gen(), None);
        assert!(!slot.eof_was_expected());
        assert_eq!(slot.probe(), (false, false), "probe on an empty slot");
        assert_eq!(slot.peek_exit_status(), None);
    }

    #[test]
    fn install_bumps_gen_and_clears_the_expected_flag() {
        let slot = ChildSlot::new();
        slot.begin_deliberate_stop();
        assert!(slot.eof_was_expected());

        slot.install(dummy_child());
        assert!(slot.is_running());
        assert_eq!(slot.generation(), 1);
        assert_eq!(slot.running_gen(), Some(1));
        assert!(
            !slot.eof_was_expected(),
            "install must reset the deliberate-stop marker unconditionally"
        );

        // Reap + wait so the test leaves no zombie behind.
        let mut child = slot.reap().expect("child was installed");
        let _ = child.wait();
    }

    #[test]
    fn reap_marks_the_eof_expected_and_empties_the_slot() {
        let slot = ChildSlot::new();
        slot.install(dummy_child());
        assert!(!slot.eof_was_expected());

        let mut child = slot.reap().expect("first reap returns the child");
        let _ = child.wait();
        assert!(
            slot.eof_was_expected(),
            "a child must never leave the slot without its EOF marked deliberate"
        );
        assert!(!slot.is_running());
        assert!(
            slot.reap().is_none(),
            "second reap: the slot is already empty"
        );
    }

    #[test]
    fn generation_survives_a_reap() {
        let slot = ChildSlot::new();
        slot.install(dummy_child());
        let mut child = slot.reap().expect("child was installed");
        let _ = child.wait();

        assert_eq!(
            slot.generation(),
            1,
            "the incarnation count outlives the child (mark_dead_if_current compares \
             against an already-emptied slot)"
        );
        assert_eq!(
            slot.running_gen(),
            None,
            "but running_gen is None once empty"
        );

        slot.install(dummy_child());
        assert_eq!(
            slot.running_gen(),
            Some(2),
            "each install keeps counting up"
        );
        let mut child = slot.reap().expect("child was installed");
        let _ = child.wait();
    }

    #[test]
    fn probe_and_peek_see_an_exited_child_without_taking_it() {
        let slot = ChildSlot::new();
        let mut child = dummy_child();
        // Let it actually exit first — `wait` caches the status in the handle, so
        // the slot's later `try_wait` peeks still see it (same trick as tts.rs's
        // exit-status reader test).
        let _ = child.wait();
        slot.install(child);

        assert_eq!(slot.probe(), (true, true), "present AND exited");
        assert_eq!(
            slot.peek_exit_status().map(|s| s.success()),
            Some(true),
            "the real exit status is readable"
        );
        assert!(
            slot.is_running(),
            "probe/peek are PEEKS — they never take the child out of the slot"
        );
        let _ = slot.reap();
    }
}
