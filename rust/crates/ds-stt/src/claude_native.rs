//! ClaudeNative — the `claude_code` STT engine: delegate dictation to Claude Code's own
//! voice via the [`Stt`] trait.
//!
//! TAP model: Claude Code's voice runs in TAP mode (`/voice tap`) — ONE keypress of
//! `voice:pushToTalk` toggles recording. `start()`/`stop()` each tap once; repeats would
//! re-toggle (the cause of "recording won't turn off").
//!
//! READ-don't-write: the key is whatever Claude Code is configured with — factory reads
//! `keybindings.json` (default `Space`) into a [`KeyChord`]. We synthesize that key via
//! `KeyInjector` and never modify Claude Code's config.
//!
//! Borrows the engine-owned platform via `Rc`; only touches `FrontmostWindow` + `KeyInjector`.
//!
//! PAIRING: only `start()` gates on a FRESH `is_terminal_frontmost()` (toggle-ON never
//! leaks outside a terminal) and remembers whether the tap fired. `stop()`/`abort()` use
//! that REMEMBERED outcome — not a fresh frontmost check. Without pairing, a focus change
//! between start and stop can send an unpaired toggle that turns Claude's recording on
//! while dontspeakd believes dictation is idle, with no UI indication.

use std::rc::Rc;

use ds_platform::{FrontmostWindow, KeyChord, KeyInjector};

use crate::Stt;

/// Claude-Code dictation engine. Generic over the platform so it can hold a shared
/// reference to the engine's single `Platform` without `unsafe impl Sync` (macOS
/// event source is `!Send`); `Stt` is non-`Send` for the same reason — single-threaded.
pub struct ClaudeNative<P: KeyInjector + FrontmostWindow> {
    platform: Rc<P>,
    /// Whether `start()` sent the toggle-ON tap. `stop()`/`abort()` pair on this
    /// REMEMBERED outcome (see module PAIRING) and clear it once consumed so a
    /// stray extra call can't re-tap.
    toggled_on: bool,
    /// Claude Code's `voice:pushToTalk` binding (config; default `Space`).
    chord: KeyChord,
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// `chord` is Claude Code's resolved dictation key; `KeyChord::default()` → `Space`.
    pub fn new(platform: Rc<P>, chord: KeyChord) -> Self {
        Self {
            platform,
            toggled_on: false,
            chord,
        }
    }
}

impl<P: KeyInjector + FrontmostWindow> ClaudeNative<P> {
    /// One focus-gated press+release. Returns whether it fired (for start→stop pairing).
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
        // Remember whether toggle-ON fired; stop() pairs on this (module PAIRING).
        self.toggled_on = self.tap();
        true
    }

    fn stop(&mut self) {
        // Matching toggle-off only if start() actually toggled on (module PAIRING).
        if self.toggled_on {
            self.platform.tap_key(&self.chord);
            self.toggled_on = false;
        }
    }

    // abort() == stop() (default): one toggle returns Claude to idle — §F long-press reset.

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
        assert_eq!(p.downs.get(), 1, "start taps Ctrl+G down when frontmost");
        assert_eq!(p.ups.get(), 1, "start completes the keypress with an up");

        // Not frontmost: keystroke must not leak outside a terminal.
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
        // start() not frontmost: toggle-ON never fires.
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(false);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.start();
        assert_eq!(p.downs.get(), 0, "start emits nothing when not frontmost");

        // Focus moves TO the terminal before stop(). Old bug: re-checking frontmost at
        // stop() would send an UNPAIRED toggle (Claude recording ON while dontspeakd
        // thinks idle). Pairing on remembered start() must prevent it.
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
        let p = Rc::new(MockPlat::default());
        p.frontmost.set(true);
        let mut e = ClaudeNative::new(p.clone(), KeyChord::default());
        e.start();
        assert_eq!(p.downs.get(), 1);

        // Focus leaves before stop(); paired toggle-off must still fire so recording
        // isn't stranded on.
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

        // Pairing already consumed; stray extra abort/stop must not re-tap.
        e.abort();
        assert_eq!(
            p.downs.get(),
            2,
            "an unpaired extra abort() must not re-tap"
        );
        assert_eq!(p.ups.get(), 2);
    }
}
