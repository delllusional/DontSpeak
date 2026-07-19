//! Mic-barge watcher: pause TTS when a FOREIGN mic goes live.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ttsq::TtsQueue;

/// Bound foreign-mic barge (~6 s) so sticky WASAPI `active` can't pause forever.
const BARGE_MAX_TICKS: u32 = 40;

/// Withhold foreign rising-edge after OUR dictation ends (~600 ms) — async `lstop` lag.
const TEARDOWN_GRACE_TICKS: u32 = 4;

/// Pure [`barge_step`] result — unit-testable without a thread or mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BargeAction {
    None,
    /// Foreign mic rising edge → pause TTS (fade + hold).
    Pause,
    /// Our barge ended (foreign idle or max ticks) → resume TTS.
    Resume,
}

/// Carry-over between ticks.
///
/// `barged`: resume only pauses *we* caused (Caps/PTT pause is owned by
/// `stop_recording`). `prev_ours`/`teardown_grace`: foreign capture that starts while
/// `ours` is true leaves no `active` edge — re-arm when `ours` drops, after up to
/// `TEARDOWN_GRACE_TICKS` of async `lstop` lag so our own teardown isn't treated as foreign.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BargeState {
    /// Prior `is_mic_active()` (rising-edge).
    prev: bool,
    /// This watcher owns the current pause.
    barged: bool,
    /// Ticks since our barge began while still active (self-heal bound).
    ticks: u32,
    /// Prior `ours` (`stt_active`) — detect OUR dictation ending for masked-foreign re-arm.
    prev_ours: bool,
    /// Ticks of post-dictation grace (0 = not in window); withholds rising-edge until
    /// `lstop` lag settles or grace exhausts.
    teardown_grace: u32,
}

/// One pure watcher tick: `active` = mic now; `ours` = our Parakeet dictation (skip barge);
/// `full_duplex` = VPIO always live (stand down).
///
/// Pause on foreign rising edge. Resume only a barge we caused once its mic idles
/// (not every idle tick — that cancelled Caps pause before requeue). Bound sticky barges.
/// On `ours` true→false: re-arm after `TEARDOWN_GRACE_TICKS` if still active (masked foreign
/// mid-dictation) rather than waiting for a false→true edge that never comes.
pub(crate) fn barge_step(
    active: bool,
    ours: bool,
    full_duplex: bool,
    st: BargeState,
    max_ticks: u32,
) -> (BargeAction, BargeState) {
    if full_duplex {
        // No edges while VPIO is always live. Unwind a pause taken before full_duplex
        // published, else the queue stays wedged; cause guard makes Resume race-safe.
        return (
            if st.barged {
                BargeAction::Resume
            } else {
                BargeAction::None
            },
            BargeState {
                prev: true,
                barged: false,
                ticks: 0,
                prev_ours: ours,
                teardown_grace: 0,
            },
        );
    }
    // Post-dictation: `active` may be our capture, a masked foreign start, or `lstop` lag.
    // Grace first; still-active after grace = treat as foreign rising edge.
    if (st.prev_ours || st.teardown_grace > 0) && !ours {
        if !active {
            // Idle within grace — re-arm, no foreign mic.
            return (
                BargeAction::None,
                BargeState {
                    prev: false,
                    prev_ours: false,
                    teardown_grace: 0,
                    ..st
                },
            );
        }
        let grace = st.teardown_grace.saturating_add(1);
        if grace < TEARDOWN_GRACE_TICKS {
            return (
                BargeAction::None,
                BargeState {
                    prev: false,
                    prev_ours: false,
                    teardown_grace: grace,
                    ..st
                },
            );
        }
        // Grace exhausted + still active → foreign.
        return (
            BargeAction::Pause,
            BargeState {
                prev: true,
                barged: true,
                ticks: 0,
                prev_ours: false,
                teardown_grace: 0,
            },
        );
    }
    if active && !st.prev && !ours {
        (
            BargeAction::Pause,
            BargeState {
                prev: active,
                barged: true,
                ticks: 0,
                prev_ours: ours,
                teardown_grace: 0,
            },
        )
    } else if st.barged && !active {
        // Only when `barged` — leaves Caps/PTT pause to `stop_recording`.
        (
            BargeAction::Resume,
            BargeState {
                prev: active,
                barged: false,
                ticks: 0,
                prev_ours: ours,
                teardown_grace: 0,
            },
        )
    } else if st.barged && !ours {
        // Sticky foreign `active` — self-heal bound.
        let ticks = st.ticks.saturating_add(1);
        if ticks >= max_ticks {
            (
                BargeAction::Resume,
                BargeState {
                    prev: active,
                    barged: false,
                    ticks: 0,
                    prev_ours: ours,
                    teardown_grace: 0,
                },
            )
        } else {
            (
                BargeAction::None,
                BargeState {
                    prev: active,
                    barged: true,
                    ticks,
                    prev_ours: ours,
                    teardown_grace: 0,
                },
            )
        }
    } else {
        (
            BargeAction::None,
            BargeState {
                prev: active,
                prev_ours: ours,
                teardown_grace: 0,
                ..st
            },
        )
    }
}

/// I/O loop: foreign mic rising edge → pause TTS. Caps dictation excluded via `stt_active`.
/// Edge-triggered + self-bounded; full-duplex stands down. Policy in [`barge_step`].
pub(crate) fn spawn_mic_barge_watcher(
    ttsq: Arc<TtsQueue>,
    stt_active: Arc<AtomicBool>,
    mic: ds_platform::MicState,
) {
    let ttsq = Arc::downgrade(&ttsq);
    std::thread::spawn(move || {
        // Cached mic watcher (CoreAudio listener / Win+Linux poll) — self-heal is tick-based.
        let mut st = BargeState::default();
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let Some(ttsq) = ttsq.upgrade() else {
                return;
            };
            // Full-duplex: `barge_step` stands down; skip the cached read.
            let full_duplex = ttsq.is_full_duplex();
            let active = if full_duplex { false } else { mic.is_active() };
            let (action, next) = barge_step(
                active,
                stt_active.load(Ordering::Relaxed),
                full_duplex,
                st,
                BARGE_MAX_TICKS,
            );
            match action {
                BargeAction::Pause => ttsq.pause_for_suspected_barge(),
                BargeAction::Resume => ttsq.resume_if_barge_speculative(),
                BargeAction::None => {}
            }
            st = next;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = 40;
    const IDLE: BargeState = BargeState {
        prev: false,
        barged: false,
        ticks: 0,
        prev_ours: false,
        teardown_grace: 0,
    };

    fn step(active: bool, ours: bool, st: BargeState) -> (BargeAction, BargeState) {
        barge_step(active, ours, false, st, MAX)
    }

    #[test]
    fn foreign_mic_rising_edge_pauses_and_marks_barged() {
        let (a, st) = step(true, false, IDLE);
        assert_eq!(a, BargeAction::Pause);
        assert!(st.barged && st.prev);
    }

    #[test]
    fn our_mic_never_barges() {
        // Caps mic rising: pause is `start_recording`'s job.
        let (a, st) = step(true, true, IDLE);
        assert_eq!(a, BargeAction::None);
        assert!(!st.barged);
    }

    #[test]
    fn idle_tick_without_a_barge_does_not_resume() {
        // Regression: non-barge idle must not resume Caps/PTT `pause_for_record` (drops held item).
        assert_eq!(step(false, false, IDLE).0, BargeAction::None);
        let mut st = IDLE;
        for _ in 0..100 {
            let (a, n) = step(false, false, st);
            assert_eq!(
                a,
                BargeAction::None,
                "idle tick must never resume a foreign-less state"
            );
            st = n;
        }
    }

    #[test]
    fn our_barge_resumes_only_when_its_mic_idles() {
        let (_, barged) = step(true, false, IDLE);
        let (a, st) = step(true, false, barged);
        assert_eq!(a, BargeAction::None);
        assert!(st.barged && st.ticks == 1);
        let (a, st) = step(false, false, st);
        assert_eq!(a, BargeAction::Resume);
        assert!(!st.barged);
    }

    #[test]
    fn ours_flips_true_mid_barge_holds_the_self_heal_counter() {
        let (a, mut st) = step(true, false, IDLE);
        assert_eq!(a, BargeAction::Pause);
        assert!(st.barged);

        // Mid-barge ours=true: self-heal arm is `!ours` — hold ticks/barged steady.
        for _ in 0..5 {
            let (a, next) = step(true, true, st);
            assert_eq!(a, BargeAction::None);
            assert!(next.barged, "still a barge we caused");
            assert_eq!(
                next.ticks, 0,
                "self-heal counter must not advance while ours=true"
            );
            st = next;
        }

        let (a, st) = step(false, true, st);
        assert_eq!(a, BargeAction::Resume);
        assert!(!st.barged);
    }

    #[test]
    fn sticky_foreign_barge_self_heals_after_max_ticks() {
        let (_, mut st) = step(true, false, IDLE);
        for _ in 0..(MAX - 1) {
            let (a, n) = step(true, false, st);
            assert_eq!(a, BargeAction::None);
            st = n;
        }
        let (a, st) = step(true, false, st);
        assert_eq!(a, BargeAction::Resume, "bounded barge self-heals");
        assert!(!st.barged && st.ticks == 0);
    }

    #[test]
    fn full_duplex_stands_down() {
        let (a, st) = barge_step(true, false, true, IDLE, MAX);
        assert_eq!(a, BargeAction::None);
        assert!(st.prev && !st.barged);
    }

    #[test]
    fn entering_full_duplex_unwinds_a_watcher_owned_pause() {
        // Race: mic active one tick before `full_duplex_active` publishes.
        let (a, barged) = step(true, false, IDLE);
        assert_eq!(a, BargeAction::Pause);

        let (a, st) = barge_step(false, false, true, barged, MAX);
        assert_eq!(a, BargeAction::Resume);
        assert!(st.prev && !st.barged);
    }

    #[test]
    fn foreign_capture_outlives_our_dictation_gets_paused_after_grace_window() {
        let (_, st) = step(true, true, IDLE);
        // Foreign joins mid-dictation: no `active` edge while `ours` is true.
        let (_, mut st) = step(true, true, st);
        for _ in 0..(TEARDOWN_GRACE_TICKS - 1) {
            let (a, next) = step(true, false, st);
            assert_eq!(
                a,
                BargeAction::None,
                "still within the teardown grace window"
            );
            st = next;
        }
        // Grace exhausted + still active → masked foreign caught.
        let (a, st) = step(true, false, st);
        assert_eq!(a, BargeAction::Pause);
        assert!(st.barged);
    }

    #[test]
    fn own_teardown_lag_does_not_false_pause_normal_dictation_end() {
        // Multi-tick `lstop` lag must not one-shot re-arm as foreign.
        let (_, mut st) = step(true, true, IDLE);
        let mut paused = false;
        for _ in 0..(TEARDOWN_GRACE_TICKS - 1) {
            let (a, next) = step(true, false, st);
            paused |= a == BargeAction::Pause;
            st = next;
        }
        let (a, st) = step(false, false, st);
        paused |= a == BargeAction::Pause;
        assert!(
            !paused,
            "own async teardown lag must never be mistaken for a foreign mic"
        );
        assert!(!st.barged);
    }
}
