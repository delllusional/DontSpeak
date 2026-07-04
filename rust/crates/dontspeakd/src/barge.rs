//! The mic-barge watcher thread that pauses TTS when a FOREIGN mic goes live.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ttsq::TtsQueue;

/// Auto-resume a foreign-mic barge after this many ticks (×150 ms ≈ 6 s) even if the
/// mic STILL reads active. A warm/foreign capture session can stay `active`
/// indefinitely (Windows WASAPI never flips it `Inactive`), which would latch
/// `is_mic_active()` true and — with a purely edge-triggered resume — wedge the queue
/// paused forever. Bounding the barge makes a stuck probe self-heal; ~6 s is long
/// enough not to chop a genuine barge.
const BARGE_MAX_TICKS: u32 = 40;

/// How many ticks (×150 ms ≈ 600 ms) to withhold foreign-mic rising-edge detection after
/// OUR dictation ends while the mic still reads active. `stop_recording`'s mic teardown
/// (the helper-process `lstop`) is async and can plausibly leave `active` reading true
/// for more than a single poll tick even with no foreign capture involved at all — so a
/// fixed one-shot re-arm is not enough; we need an actual bounded window sized to ride
/// out that lag. 600 ms is comfortably longer than the teardown lag observed in practice
/// while still catching a genuinely overlapping foreign mic promptly once it expires.
const TEARDOWN_GRACE_TICKS: u32 = 4;

/// What a single watcher tick decides to do to the TTS queue. PURE result of
/// [`barge_step`], so the whole policy is unit-testable without a thread or a mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BargeAction {
    /// Do nothing this tick.
    None,
    /// A foreign mic just went live → pause our TTS (fade + hold the queue).
    Pause,
    /// Our barge is over (foreign mic idle, or bounded out) → resume our TTS.
    Resume,
}

/// The watcher's carry-over state between ticks. `barged` is the crux of the
/// dropped-narration fix: we only ever `Resume` a pause WE caused, so a Caps/PTT pause
/// (owned by `stop_recording`) is never clobbered here. `prev_ours`/`teardown_grace` are
/// the crux of the masked-foreign-mic fix: a foreign capture that starts while `ours` was
/// already true left no `active` edge to see, so we re-arm edge detection the moment
/// `ours` drops instead of waiting for a `false→true` edge that will never come while it
/// stays continuously active — but we ride out our own device's async teardown lag for
/// up to `TEARDOWN_GRACE_TICKS` first, so we don't mistake OUR OWN teardown for a
/// foreign mic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BargeState {
    /// `is_mic_active()` last tick — for rising-edge detection.
    prev: bool,
    /// Did THIS watcher pause the queue (a foreign-mic barge)?
    barged: bool,
    /// Ticks elapsed since our still-active barge began (self-heal bound).
    ticks: u32,
    /// `ours` (stt_active) last tick — lets the watcher notice OUR dictation ending, the
    /// crux of the masked-foreign-mic fix: a foreign capture that starts while `ours` was
    /// already true left no `active` edge to see, so we re-arm edge detection the moment
    /// `ours` drops instead of waiting for a `false→true` edge that will never come while
    /// it stays continuously active.
    prev_ours: bool,
    /// Ticks elapsed since OUR dictation ended while the mic still reads active — bounds
    /// how long we withhold rising-edge detection to ride out our own device's async
    /// teardown lag (`stop_recording`'s helper-process `lstop`) before trusting a
    /// still-active mic as genuinely foreign. 0 means we are not currently in this
    /// post-dictation grace window.
    teardown_grace: u32,
}

/// Decide one watcher tick from the live signals + carry-over state. PURE.
///
/// - `active`: `is_mic_active()` now.   - `ours`: the mic is OUR Parakeet dictation
///   (`stt_active`) — never barge it.   - `full_duplex`: the VPIO mic is always live,
///   so edge detection is meaningless; stand the watcher down.
///
/// Rules: pause on a FOREIGN rising edge; resume ONLY a barge we caused once its mic
/// idles (NOT on every idle tick — that was the bug that cancelled a Caps pause before
/// the worker could requeue, dropping the held item); bound an our-barge whose mic
/// never idles so a sticky session can't wedge the queue; the moment OUR dictation ends
/// (`ours`: true → false), re-arm edge detection instead of waiting for a `false→true`
/// edge on `active` that will never come if a foreign capture started mid-dictation and
/// has stayed continuously active — but withhold judgement for up to
/// `TEARDOWN_GRACE_TICKS` first, so our own device's async teardown lag is never
/// mistaken for a foreign mic.
pub(crate) fn barge_step(
    active: bool,
    ours: bool,
    full_duplex: bool,
    st: BargeState,
    max_ticks: u32,
) -> (BargeAction, BargeState) {
    if full_duplex {
        // Mic permanently live → no edges; never barge, and forget any prior barge.
        return (
            BargeAction::None,
            BargeState {
                prev: true,
                barged: false,
                ticks: 0,
                prev_ours: ours,
                teardown_grace: 0,
            },
        );
    }
    // Our own dictation just ended (`ours`: true → false), or we're still riding out the
    // grace window that began when it did. An `active` reading here is uninformative
    // about a FOREIGN mic: it may be our own capture (including a foreign one that
    // started mid-dictation and left no edge to see), or just our own device's async
    // teardown (`stop_recording`'s helper-process `lstop`) lingering `active` for a tick
    // or more after `stop_recording` already flipped `ours`/resumed the queue. Ride out
    // up to `TEARDOWN_GRACE_TICKS` of that lag before trusting a still-active mic as
    // genuinely foreign, so the normal, non-overlapping end of every dictation never
    // false-pauses — but once the window is exhausted and the mic is STILL active, treat
    // it as a fresh rising edge: if a foreign capture really does outlive ours, it gets
    // caught here instead of never (a purely edge-triggered scheme would see no edge at
    // all while `active` stays continuously true through the `ours` transition).
    if (st.prev_ours || st.teardown_grace > 0) && !ours {
        if !active {
            // Settled idle within the window — nothing was foreign; simply re-arm.
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
            // Still within the grace window: keep withholding judgement.
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
        // Grace exhausted and STILL active → genuinely foreign now.
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
        // Foreign mic rising edge → pause OUR TTS, and remember WE did it.
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
        // Our barge's foreign mic went idle → resume. (Only `st.barged` — a non-barge
        // idle tick does nothing, so a Caps/PTT pause is left for `stop_recording`.)
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
        // Our barge but the mic still reads active (sticky/foreign) → count toward the
        // self-heal bound so a never-idle probe can't wedge the queue paused.
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
        // Nothing to do — just advance the edge memory.
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

/// Watch the mic and barge the engine's TTS on the idle→active EDGE of a FOREIGN mic,
/// so speech stops when another recorder (Claude Code's own voice input, another app)
/// goes live. Caps dictation is excluded via `stt_active` (`ours`) and already barges
/// on the tap. Edge-triggered + self-bounded; half-duplex only (stands down in
/// full-duplex). All policy lives in the pure [`barge_step`]; this is just the I/O loop.
pub(crate) fn spawn_mic_barge_watcher(
    ttsq: Arc<TtsQueue>,
    stt_active: Arc<AtomicBool>,
    mic: ds_platform::MicState,
) {
    std::thread::spawn(move || {
        // Reads the shared mic watcher's CACHED state (a native CoreAudio property listener
        // on macOS, a centralized poll thread on Windows/Linux) — no per-tick device query.
        // The state machine still ticks because its self-heal bound is tick-based.
        let mut st = BargeState::default();
        loop {
            std::thread::sleep(Duration::from_millis(150));
            // In full-duplex the VPIO mic is permanently live, so `barge_step` stands down
            // and ignores `active` entirely — skip even the cached read.
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
                BargeAction::Pause => ttsq.pause_for_record(),
                BargeAction::Resume => ttsq.resume(),
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
        // Caps dictation mic (ours) rising → NOTHING; the pause is start_recording's job.
        let (a, st) = step(true, true, IDLE);
        assert_eq!(a, BargeAction::None);
        assert!(!st.barged);
    }

    #[test]
    fn idle_tick_without_a_barge_does_not_resume() {
        // THE REGRESSION GUARD: a non-barge idle tick must NOT resume — else it cancels
        // a Caps/PTT pause (pause_for_record) before the worker requeues, dropping the
        // held narration. `barged=false` (we didn't pause) → no resume, ever.
        assert_eq!(step(false, false, IDLE).0, BargeAction::None);
        // Even repeated idle ticks stay silent.
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
        // Foreign edge → pause (barged).
        let (_, barged) = step(true, false, IDLE);
        // Mic still active next tick → still nothing (just counts).
        let (a, st) = step(true, false, barged);
        assert_eq!(a, BargeAction::None);
        assert!(st.barged && st.ticks == 1);
        // Mic idles → resume, barged cleared.
        let (a, st) = step(false, false, st);
        assert_eq!(a, BargeAction::Resume);
        assert!(!st.barged);
    }

    #[test]
    fn ours_flips_true_mid_barge_holds_the_self_heal_counter() {
        // Foreign mic rising edge → pause (barged=true, ticks=0).
        let (a, mut st) = step(true, false, IDLE);
        assert_eq!(a, BargeAction::Pause);
        assert!(st.barged);

        // The mic then reads as OURS while still active (e.g. our own dictation starts
        // mid-barge) — this falls through to the catch-all else arm (the self-heal arm
        // is guarded `!ours`), which must hold `ticks`/`barged` steady rather than
        // advancing the self-heal counter.
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

        // Mic idles → still resumes via `barged && !active`, unaffected by `ours`.
        let (a, st) = step(false, true, st);
        assert_eq!(a, BargeAction::Resume);
        assert!(!st.barged);
    }

    #[test]
    fn sticky_foreign_barge_self_heals_after_max_ticks() {
        // Foreign edge → pause.
        let (_, mut st) = step(true, false, IDLE);
        // Mic stays active forever (sticky session): count up to the bound, then resume.
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
        // Even a foreign rising edge does nothing in full-duplex; prev latches true.
        let (a, st) = barge_step(true, false, true, IDLE, MAX);
        assert_eq!(a, BargeAction::None);
        assert!(st.prev && !st.barged);
    }

    #[test]
    fn foreign_capture_outlives_our_dictation_gets_paused_after_grace_window() {
        // Our dictation starts, mic active.
        let (_, st) = step(true, true, IDLE);
        // A foreign capture joins mid-dictation — no `active` edge is visible while
        // `ours` is still true.
        let (_, mut st) = step(true, true, st);
        // Our dictation ends; the (masked, foreign) mic stays active. Withhold judgement
        // through the whole teardown-grace window...
        for _ in 0..(TEARDOWN_GRACE_TICKS - 1) {
            let (a, next) = step(true, false, st);
            assert_eq!(
                a,
                BargeAction::None,
                "still within the teardown grace window"
            );
            st = next;
        }
        // ...but once the window is exhausted and the mic is STILL active, it's
        // genuinely foreign: the masked capture finally gets caught.
        let (a, st) = step(true, false, st);
        assert_eq!(a, BargeAction::Pause);
        assert!(st.barged);
    }

    #[test]
    fn own_teardown_lag_does_not_false_pause_normal_dictation_end() {
        // Our dictation starts and ends normally; no foreign mic is ever involved. Our
        // own device takes SEVERAL ticks (helper-process `lstop`) to actually settle
        // idle — more than a single tick, which a fixed one-shot re-arm would have
        // mistaken for a foreign rising edge on the very next poll.
        let (_, mut st) = step(true, true, IDLE);
        let mut paused = false;
        for _ in 0..(TEARDOWN_GRACE_TICKS - 1) {
            let (a, next) = step(true, false, st);
            paused |= a == BargeAction::Pause;
            st = next;
        }
        // Our own device finally settles idle, still within the grace window.
        let (a, st) = step(false, false, st);
        paused |= a == BargeAction::Pause;
        assert!(
            !paused,
            "own async teardown lag must never be mistaken for a foreign mic"
        );
        assert!(!st.barged);
    }
}
