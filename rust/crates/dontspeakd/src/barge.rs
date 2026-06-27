//! The mic-barge watcher thread that pauses TTS when a FOREIGN mic goes live.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ttsq::TtsQueue;

/// Auto-resume a foreign-mic barge after this many ticks (×150 ms ≈ 6 s) even if the
/// mic STILL reads active. A warm/foreign capture session can stay `active`
/// indefinitely (Windows WASAPI never flips it `Inactive`), which would latch
/// `mic_active()` true and — with a purely edge-triggered resume — wedge the queue
/// paused forever. Bounding the barge makes a stuck probe self-heal; ~6 s is long
/// enough not to chop a genuine barge.
const BARGE_MAX_TICKS: u32 = 40;

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
/// (owned by `stop_recording`) is never clobbered here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BargeState {
    /// `mic_active()` last tick — for rising-edge detection.
    prev: bool,
    /// Did THIS watcher pause the queue (a foreign-mic barge)?
    barged: bool,
    /// Ticks elapsed since our still-active barge began (self-heal bound).
    ticks: u32,
}

/// Decide one watcher tick from the live signals + carry-over state. PURE.
///
/// - `active`: `mic_active()` now.   - `ours`: the mic is OUR Parakeet dictation
///   (`stt_active`) — never barge it.   - `full_duplex`: the VPIO mic is always live,
///   so edge detection is meaningless; stand the watcher down.
///
/// Rules: pause on a FOREIGN rising edge; resume ONLY a barge we caused once its mic
/// idles (NOT on every idle tick — that was the bug that cancelled a Caps pause before
/// the worker could requeue, dropping the held item); bound an our-barge whose mic
/// never idles so a sticky session can't wedge the queue.
pub(crate) fn barge_step(
    active: bool,
    ours: bool,
    full_duplex: bool,
    st: BargeState,
    max_ticks: u32,
) -> (BargeAction, BargeState) {
    if full_duplex {
        // Mic permanently live → no edges; never barge, and forget any prior barge.
        return (BargeAction::None, BargeState { prev: true, barged: false, ticks: 0 });
    }
    if active && !st.prev && !ours {
        // Foreign mic rising edge → pause OUR TTS, and remember WE did it.
        (BargeAction::Pause, BargeState { prev: active, barged: true, ticks: 0 })
    } else if st.barged && !active {
        // Our barge's foreign mic went idle → resume. (Only `st.barged` — a non-barge
        // idle tick does nothing, so a Caps/PTT pause is left for `stop_recording`.)
        (BargeAction::Resume, BargeState { prev: active, barged: false, ticks: 0 })
    } else if st.barged && !ours {
        // Our barge but the mic still reads active (sticky/foreign) → count toward the
        // self-heal bound so a never-idle probe can't wedge the queue paused.
        let ticks = st.ticks.saturating_add(1);
        if ticks >= max_ticks {
            (BargeAction::Resume, BargeState { prev: active, barged: false, ticks: 0 })
        } else {
            (BargeAction::None, BargeState { prev: active, barged: true, ticks })
        }
    } else {
        // Nothing to do — just advance the edge memory.
        (BargeAction::None, BargeState { prev: active, ..st })
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
    const IDLE: BargeState = BargeState { prev: false, barged: false, ticks: 0 };

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
            assert_eq!(a, BargeAction::None, "idle tick must never resume a foreign-less state");
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
    fn no_double_pause_while_foreign_mic_stays_active() {
        // Rising edge pauses once; subsequent active ticks (prev=true) never re-pause.
        let (_, st) = step(true, false, IDLE);
        let (a, _) = step(true, false, st);
        assert_eq!(a, BargeAction::None);
    }
}
