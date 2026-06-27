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

use std::rc::Rc;

use ds_platform::{FrontmostWindow, KeyChord, KeyInjector};

use crate::Stt;

/// The Claude-Code-dictation engine. Generic over the platform so it can hold a shared
/// reference to the engine's single `Platform` instance without an `unsafe impl Sync`
/// (the macOS event source is `!Send`); `Stt` is non-`Send` for the same reason — the
/// engine is single-threaded.
pub struct ClaudeNative<P: KeyInjector + FrontmostWindow> {
    plat: Rc<P>,
    holding: bool,
    /// The key Claude Code's `voice:pushToTalk` is bound to (read from its config; default
    /// `Space`). Tapped on each start/stop toggle.
    chord: KeyChord,
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// `chord` is Claude Code's resolved dictation key (see [`KeyChord`]); pass
    /// `KeyChord::default()` for the default `Space`.
    pub fn new(plat: Rc<P>, chord: KeyChord) -> Self {
        Self {
            plat,
            holding: false,
            chord,
        }
    }
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// Tap Claude Code's dictation key ONCE (a complete press+release), focus-gated so the
    /// keystroke never leaks outside a terminal. This is the single toggle Claude Code's
    /// voice TAP mode expects: one tap toggles recording.
    fn tap(&self) {
        if self.plat.terminal_frontmost() {
            self.plat.tap_key(&self.chord);
        }
    }
}

impl<P: KeyInjector + FrontmostWindow> Stt for ClaudeNative<P> {
    fn start(&mut self) -> bool {
        // Start TAP: one tap toggles Claude Code's voice recording ON.
        self.holding = true;
        self.tap();
        true
    }

    fn stop(&mut self) {
        // Stop TAP: one tap toggles recording OFF (Claude submits/inserts).
        self.holding = false;
        self.tap();
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
        fn terminal_frontmost(&self) -> bool {
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
    fn stop_and_abort_each_tap_once() {
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.stop();
        assert_eq!(p.downs.get(), 1, "stop taps Ctrl+G to toggle recording off");
        assert_eq!(p.ups.get(), 1, "stop completes the keypress");
        e.abort();
        assert_eq!(
            p.downs.get(),
            2,
            "abort also taps once (no transcript to discard)"
        );
        assert_eq!(p.ups.get(), 2);
    }
}
