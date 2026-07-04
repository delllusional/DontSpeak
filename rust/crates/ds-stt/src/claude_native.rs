//! ClaudeNative — the `claude_code` STT engine: delegate dictation to Claude Code's own
//! voice, through the [`Stt`] trait.
//!
//! TAP model: Claude Code's voice runs in TAP mode (`/voice tap`), where ONE keypress of
//! its `voice:pushToTalk` key toggles recording. So `start()` and `stop()` each tap that
//! key ONCE; sending repeats would re-toggle recording (the cause of "recording won't
//! turn off"), so there are none.
//!
//! READ-don't-write: the key is whatever Claude Code is configured with — read from its
//! `keybindings.json` (default `Space`) into a [`KeyChord`] by the factory and handed in
//! here. We synthesize exactly that key (via the platform `KeyInjector`) and never modify
//! Claude Code's config.
//!
//! It borrows the platform the engine already owns (via an `Rc`), and only touches the
//! `FrontmostWindow` focus gate + the `KeyInjector` tap.
//!
//! PAIRING: `start()` is the only call gated on a FRESH `is_terminal_frontmost()` check (so
//! the toggle-ON tap never leaks outside a terminal); it remembers whether that tap
//! actually fired. `stop()`/`abort()` key off that REMEMBERED outcome instead of
//! independently re-checking frontmost state — if `start()` never toggled recording on,
//! `stop()` must not tap either. Without this pairing, a focus change between `start()`
//! and `stop()` could send an unpaired toggle keystroke that turns Claude Code's own
//! recording on while dontspeakd believes dictation is idle, with no UI indication.

use std::rc::Rc;

use ds_platform::{FrontmostWindow, KeyChord, KeyInjector};

use crate::Stt;

/// The Claude-Code-dictation engine. Generic over the platform so it can hold a shared
/// reference to the engine's single `Platform` instance without an `unsafe impl Sync`
/// (the macOS event source is `!Send`); `Stt` is non-`Send` for the same reason — the
/// engine is single-threaded.
pub struct ClaudeNative<P: KeyInjector + FrontmostWindow> {
    platform: Rc<P>,
    /// Whether `start()` actually sent the toggle-ON tap (the terminal was frontmost at
    /// start time). `stop()`/`abort()` pair against this REMEMBERED outcome rather than a
    /// fresh frontmost check — see the module-level PAIRING note — and clear it once
    /// consumed so a stray extra `stop()`/`abort()` can't re-tap.
    toggled_on: bool,
    /// The key Claude Code's `voice:pushToTalk` is bound to (read from its config; default
    /// `Space`). Tapped on each start/stop toggle.
    chord: KeyChord,
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// `chord` is Claude Code's resolved dictation key (see [`KeyChord`]); pass
    /// `KeyChord::default()` for the default `Space`.
    pub fn new(platform: Rc<P>, chord: KeyChord) -> Self {
        Self {
            platform,
            toggled_on: false,
            chord,
        }
    }
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// Tap Claude Code's dictation key ONCE (a complete press+release), focus-gated so the
    /// keystroke never leaks outside a terminal. This is the single toggle Claude Code's
    /// voice TAP mode expects: one tap toggles recording. Returns whether it actually
    /// fired, so `start()` can remember the outcome for `stop()` to pair against.
    fn tap(&self) -> bool {
        let frontmost = self.platform.is_terminal_frontmost();
        if frontmost {
            self.platform.tap_key(&self.chord);
        }
        frontmost
    }
}

impl<P: KeyInjector + FrontmostWindow> Stt for ClaudeNative<P> {
    fn start(&mut self) -> bool {
        // Start TAP: one tap toggles Claude Code's voice recording ON, gated on the
        // terminal being frontmost right now. Remember whether it actually fired —
        // `stop()` mirrors this instead of independently re-checking frontmost state.
        self.toggled_on = self.tap();
        true
    }

    fn stop(&mut self) {
        // Stop TAP: send the matching toggle-off ONLY if start() actually toggled
        // recording on, keyed off that remembered pairing rather than a fresh
        // frontmost check — see the module-level PAIRING note. A focus change
        // between start() and stop() can therefore never produce an unpaired
        // toggle: if start() never fired, stop() won't either, and a stray extra
        // stop()/abort() call (pairing already consumed) won't re-tap.
        if self.toggled_on {
            self.platform.tap_key(&self.chord);
            self.toggled_on = false;
        }
    }

    // abort() == stop() (the default): a single toggle returns Claude to idle,
    // which is exactly the §F long-press reset semantics for ClaudeNative.

    fn is_available(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "claude_code"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct MockPlat {
        frontmost: Cell<bool>,
        downs: Cell<u32>,
        ups: Cell<u32>,
    }
    impl KeyInjector for MockPlat {
        // A tap is one press+release, so it bumps both the down and up counters.
        fn tap_key(&self, _chord: &KeyChord) {
            self.downs.set(self.downs.get() + 1);
            self.ups.set(self.ups.get() + 1);
        }
    }
    impl FrontmostWindow for MockPlat {
        fn is_terminal_frontmost(&self) -> bool {
            self.frontmost.get()
        }
    }

    #[test]
    fn start_taps_once_when_frontmost() {
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        assert!(e.start());
        // One complete keypress = one TAP toggle.
        assert_eq!(p.downs.get(), 1, "start taps Ctrl+G down when frontmost");
        assert_eq!(p.ups.get(), 1, "start completes the keypress with an up");

        // Not frontmost: no emit (the keystroke must not leak outside a terminal).
        let p2 = Rc::new(MockPlat::default());
        p2.frontmost.set(false);
        let mut e2 = ClaudeNative::new(p2.clone(), KeyChord::default());
        e2.start();
        assert_eq!(p2.downs.get(), 0, "no emit when focus is elsewhere");
        assert_eq!(p2.ups.get(), 0, "no emit when focus is elsewhere");
    }

    #[test]
    fn stop_taps_the_matching_toggle_off_when_paired_with_start() {
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        assert!(e.start());
        e.stop();
        assert_eq!(
            p.downs.get(),
            2,
            "start's toggle-on and stop's matching toggle-off each tap once"
        );
        assert_eq!(p.ups.get(), 2);
    }

    #[test]
    fn stop_does_not_send_an_unpaired_toggle_when_start_never_fired() {
        // start() while NOT frontmost: the toggle-ON tap never fires, so Claude Code's
        // recording never actually turns on.
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(false);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.start();
        assert_eq!(p.downs.get(), 0, "start emits nothing when not frontmost");

        // Focus now moves TO the terminal before stop() runs. Re-checking frontmost at
        // stop() (the old bug) would tap here — an UNPAIRED toggle that turns Claude's
        // own recording ON while dontspeakd believes dictation is idle, with no UI
        // indication. Pairing on the remembered start() outcome must prevent it.
        p.frontmost.set(true);
        e.stop();
        assert_eq!(
            p.downs.get(),
            0,
            "stop must not tap when start() never toggled recording on"
        );
        assert_eq!(p.ups.get(), 0);
    }

    #[test]
    fn stop_still_sends_the_matching_toggle_off_after_focus_leaves_the_terminal() {
        // start() while frontmost: the toggle-ON tap fires, recording is now on.
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.start();
        assert_eq!(p.downs.get(), 1);

        // Focus leaves the terminal before stop() runs. The paired toggle-off must
        // still fire — stop() no longer independently re-decides from a fresh
        // frontmost check, so a focus change can't strand recording on.
        p.frontmost.set(false);
        e.stop();
        assert_eq!(
            p.downs.get(),
            2,
            "stop must still send the matching toggle-off despite the focus change"
        );
        assert_eq!(p.ups.get(), 2);
    }

    #[test]
    fn abort_taps_once_when_paired_and_a_stray_extra_call_does_not_retap() {
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.start();
        e.abort(); // default delegates to stop(): the one matching toggle-off tap.
        assert_eq!(
            p.downs.get(),
            2,
            "start's toggle-on and abort's matching toggle-off each tap once"
        );
        assert_eq!(p.ups.get(), 2);

        // The pairing was already consumed by the abort() above; a stray extra
        // abort()/stop() call must not re-tap.
        e.abort();
        assert_eq!(
            p.downs.get(),
            2,
            "an unpaired extra abort() must not re-tap"
        );
        assert_eq!(p.ups.get(), 2);
    }
}
