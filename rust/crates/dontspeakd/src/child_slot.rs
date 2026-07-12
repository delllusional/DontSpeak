//! `ChildSlot` — the warm `ds-helper` child's process-lifecycle slot.
//!
//! `tts.rs` used to track the child's lifecycle as three independent primitives —
//! a `Mutex<Option<Child>>` handle, an `AtomicU64` incarnation counter, and an
//! `AtomicBool` deliberate-teardown marker — kept coherent by hand at every call
//! site. Collapsing them into one type turns three convention-locked couplings
//! into structure:
//!
//! 1. *"bump the generation WHILE holding the `child` lock"* — the generation now
//!    lives INSIDE the same mutex as the handle, so [`ChildSlot::install`] is one
//!    atomic write (formerly a comment-enforced block at the `start_locked` call
//!    site).
//! 2. *"reset `expected_eof` on every successful start"* — [`ChildSlot::install`]
//!    does it unconditionally (formerly a separate store a future edit could drop).
//! 3. *"set `expected_eof` before killing"* — [`ChildSlot::reap`] re-asserts the
//!    flag before taking the child, so it is impossible to remove a child from the
//!    slot without marking its EOF deliberate.

use std::process::Child;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The warm ds-helper child's process-lifecycle slot: the live handle, its
/// incarnation number, and the deliberate-teardown marker — one type so the three
/// can no longer drift apart by convention (see the module doc's couplings).
///
/// Locking discipline: every method holds the cell mutex only briefly (O(1) work
/// plus a non-blocking `try_wait`) — never across `kill()`/`wait()` (those happen
/// on the value [`reap`](Self::reap) returns, outside any lock), never across a
/// `Condvar` wait (the slot contains none). Callers serialize lifecycle
/// TRANSITIONS with `TtsManager`'s outer `lifecycle` lock; this mutex is the
/// innermost.
pub(crate) struct ChildSlot {
    /// The handle and its generation INSIDE one mutex: installing a child and
    /// bumping its incarnation are one atomic write, so anyone who next observes
    /// the child is guaranteed (by the mutex's own happens-before edge) to see the
    /// new generation too.
    cell: Mutex<ChildCell>,
    /// Deliberate-teardown marker. Stays an ATOMIC — not a mutex-guarded variant —
    /// on purpose: the reader thread classifies an EOF lock-free after the pipe
    /// closes ([`eof_was_expected`](Self::eof_was_expected)), and must never block
    /// behind a teardown path that is mid `kill()`/`wait()`.
    expected_eof: AtomicBool,
}

struct ChildCell {
    /// The live `ds-helper --serve` child (`None` when not warm).
    child: Option<Child>,
    /// The child's incarnation number, bumped by every [`ChildSlot::install`].
    /// (Named `generation` in full — `gen` is a reserved keyword in edition 2024.)
    generation: u64,
}

impl ChildSlot {
    /// An empty slot: no child, generation 0, EOF not expected.
    pub(crate) fn new() -> Self {
        Self {
            cell: Mutex::new(ChildCell {
                child: None,
                generation: 0,
            }),
            expected_eof: AtomicBool::new(false),
        }
    }

    /// Install a freshly READY child: handle + generation bump + expected-EOF reset
    /// as ONE transition. Call only under the caller's `lifecycle` lock, after the
    /// previous reader thread has been joined and before the new one is spawned —
    /// no reader exists in that window, so folding the flag reset in here is
    /// order-insensitive. From this point an EOF is a CRASH unless a deliberate
    /// teardown ([`begin_deliberate_stop`](Self::begin_deliberate_stop)) re-marks
    /// it expected before killing.
    pub(crate) fn install(&self, child: Child) {
        {
            let mut cell = self.cell.lock().unwrap();
            cell.child = Some(child);
            cell.generation += 1;
        }
        self.expected_eof.store(false, Ordering::Release);
    }

    /// Mark the teardown that is about to happen DELIBERATE, so the reader doesn't
    /// report the resulting EOF as a crash. Call FIRST on a teardown path — before
    /// the stdin drop that can already make the child exit.
    pub(crate) fn begin_deliberate_stop(&self) {
        self.expected_eof.store(true, Ordering::Release);
    }

    /// Take the child out of the slot for the caller to kill/wait/log — OUTSIDE any
    /// lock (this method has already released the cell mutex when it returns).
    /// Re-asserts the deliberate-teardown marker first (idempotent belt-and-braces —
    /// both teardown paths already ran [`begin_deliberate_stop`](Self::begin_deliberate_stop)),
    /// so a child can never leave the slot without its EOF being marked deliberate.
    pub(crate) fn reap(&self) -> Option<Child> {
        self.expected_eof.store(true, Ordering::Release);
        self.cell.lock().unwrap().child.take()
    }

    /// True when a warm child is installed.
    pub(crate) fn is_running(&self) -> bool {
        self.cell.lock().unwrap().child.is_some()
    }

    /// The current generation of the RUNNING child, or `None` when the slot is
    /// empty — one acquisition for the callers (`play`) that need both facts.
    pub(crate) fn running_gen(&self) -> Option<u64> {
        let cell = self.cell.lock().unwrap();
        cell.child.is_some().then_some(cell.generation)
    }

    /// The current generation, whether or not a child is installed — for
    /// `mark_dead_if_current`'s staleness compare (which must also match against
    /// an already-emptied slot).
    pub(crate) fn generation(&self) -> u64 {
        self.cell.lock().unwrap().generation
    }

    /// `(present, exited)`: is a child installed, and has it already exited?
    /// `try_wait` Err ⇒ treat as exited — the handle is unusable either way. Feeds
    /// [`crate::config_gate::warm_child_heal_action`] unchanged. Peek only: never
    /// takes/kills the child.
    pub(crate) fn probe(&self) -> (bool, bool) {
        match self.cell.lock().unwrap().child.as_mut() {
            Some(c) => (true, !matches!(c.try_wait(), Ok(None))),
            None => (false, false),
        }
    }

    /// Was the EOF the reader just saw marked deliberate? Lock-free (atomic load) —
    /// the reader's EOF classification must never block behind a teardown path
    /// that is mid `kill()`/`wait()`.
    pub(crate) fn eof_was_expected(&self) -> bool {
        self.expected_eof.load(Ordering::Acquire)
    }

    /// Peek the child's exit status if it has already exited (`None` when the slot
    /// is empty, the child is still alive, or `try_wait` errs). Peek only — never
    /// takes/kills, so a later `mark_dead`/`restart_if_crashed` still owns the
    /// actual reap.
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
