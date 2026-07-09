//! macOS platform impl. Compile-verified on Apple Silicon (host build).
//!
//! * Caps key state: the physical key down/up via `IOHIDManager` (`iohid.rs`),
//!   the robust path — the IOKit lock-state read never tracks toggles on this
//!   host's external keyboard. IOKit (`iokit.rs`) is kept only for the §F LED
//!   WRITE. (The IOHIDManager read needs only the Accessibility grant — which
//!   subsumes Input Monitoring; see `iohid.rs`.)
//! * Dictation key: `core-graphics` `CGEvent` keyboard events (modifiers carried
//!   as flags on the base key), posted to the session event tap.
//! * Frontmost app: `NSWorkspace.frontmostApplication.bundleIdentifier` via
//!   objc2-app-kit, matched against [`crate::KNOWN_TERMINALS`].
//! * Preflight: `AXIsProcessTrusted()` (read-only, no prompt).

mod capskey;
mod iohid;
mod iokit;
mod led;
mod stuck_grant;

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use objc2_app_kit::NSWorkspace;

use crate::{
    CapsKeyMonitor, FrontmostWindow, KNOWN_TERMINALS, KeyBase, KeyChord, KeyInjector, Platform,
    PreflightError,
};

/// kVK_ANSI_V — for the synthetic Cmd+V paste in `type_text` (§C.3).
const KEY_V: u16 = 9;
/// kVK_Return — the Enter key the always-listening loop presses to submit.
const KEY_RETURN: u16 = 36;

/// Map a [`KeyBase`] to its macOS kVK virtual keycode. Letters use the US-ANSI layout
/// (non-sequential keycodes). `None` for `Unsupported` — the caller logs + skips.
fn mac_keycode(base: &KeyBase) -> Option<u16> {
    Some(match base {
        KeyBase::Space => 49,
        KeyBase::Enter => 36,
        KeyBase::Tab => 48,
        KeyBase::Escape => 53,
        KeyBase::Letter(c) => match c.to_ascii_lowercase() {
            'a' => 0,
            'b' => 11,
            'c' => 8,
            'd' => 2,
            'e' => 14,
            'f' => 3,
            'g' => 5,
            'h' => 4,
            'i' => 34,
            'j' => 38,
            'k' => 40,
            'l' => 37,
            'm' => 46,
            'n' => 45,
            'o' => 31,
            'p' => 35,
            'q' => 12,
            'r' => 15,
            's' => 1,
            't' => 17,
            'u' => 32,
            'v' => 9,
            'w' => 13,
            'x' => 7,
            'y' => 16,
            'z' => 6,
            _ => return None,
        },
        KeyBase::Unsupported(_) => return None,
    })
}

#[cfg(test)]
// Parity test deliberately co-located with the keycode map above; the platform impls
// intentionally follow it in this file.
#[allow(clippy::items_after_test_module)]
mod keycode_parity {
    use super::*;
    use crate::chord::all_supported_bases;

    #[test]
    fn every_supported_base_maps_to_a_keycode() {
        for b in all_supported_bases() {
            assert!(
                mac_keycode(&b).is_some(),
                "macOS mac_keycode has no kVK for {b:?}"
            );
        }
    }

    #[test]
    fn unsupported_base_has_no_keycode() {
        assert!(mac_keycode(&KeyBase::Unsupported("f5".into())).is_none());
    }
}

pub struct MacOsPlatform {
    caps: iokit::CapsReader,
    /// Direct physical-LED writer (HID Manager). Drives the Caps-Lock LED on every
    /// keyboard, decoupled from the logical lock — the part `iokit`'s lock-coupled
    /// write can't reliably do on external/Bluetooth keyboards. Retries opening
    /// itself (throttled) while missing — e.g. Accessibility not yet granted at
    /// construction time — instead of giving up for the process's whole life; see
    /// `led::RetryingCapsLed`. Until it's open, only the lock-state write drives
    /// the LED.
    led: led::RetryingCapsLed,
    /// The CGEventSource the dictation tap is posted through. MUST be `HIDSystemState` so a
    /// synthesized keypress can't clobber the Caps-Lock LED the engine drives as its recording
    /// indicator. (A key posted through a DIFFERENT source flips the Caps lock/LED → a spurious
    /// toggle ~120 ms after each start; this coupling is exactly why key injection here can't
    /// be generic.)
    source: CGEventSource,
    /// Physical Caps-key down state, published by the IOHIDManager monitor thread
    /// (`iohid::spawn_caps_hid_monitor`). Read synchronously by `read()` (HOLD
    /// trigger) and `is_caps_physically_down()` (§F long-press) from the engine's
    /// poll thread.
    caps_down: Arc<AtomicBool>,
    /// Whether THIS instance currently owns the Caps key (last call was
    /// `acquire_caps_key`, not yet followed by `release_caps_key`). Unlike Linux's
    /// persistent marker file, `hidutil`'s `UserKeyMapping` is a single global, and
    /// `capskey::release_caps_key()` clears the WHOLE thing unconditionally — with no
    /// guard, a `release_caps_key()` call when we never acquired (e.g. `caps_enabled`
    /// was false all session, or a double release) would wipe a user's own unrelated
    /// hidutil remap. Gating on this in-process flag is enough here (no persistence
    /// needed): the remap is per-login anyway, so there's nothing to reconcile across
    /// restarts the way Linux's GNOME/KDE settings need.
    owns_key: Cell<bool>,
    /// User config.toml `extra_terminals` — extends `KNOWN_TERMINALS` at lookup time.
    /// Same single-poll-thread reasoning as `owns_key` above (`Rc<MacOsPlatform>` is
    /// `!Send`, so plain interior mutability suffices).
    extra_terminals: RefCell<Vec<String>>,
}

impl MacOsPlatform {
    pub fn new() -> Result<Self, PreflightError> {
        let caps = iokit::CapsReader::open()
            .ok_or_else(|| PreflightError("cannot open IOHIDSystem".into()))?;
        // .hidSystemState source: session-level events that flow to the focused app's PTY,
        // AND the source whose Caps-Lock LED the engine drives — so the dictation tap can't
        // clobber the recording indicator.
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| PreflightError("cannot create CGEventSource".into()))?;
        let caps_down = Arc::new(AtomicBool::new(false));
        // Physical Caps-key down/up via IOHIDManager — the robust HOLD signal,
        // replacing the lock-coupled CGEvent AlphaShift tap that was blind on this
        // machine. See `iohid.rs` for the IOKit-vs-IOHIDManager rationale + Input
        // Monitoring permission note.
        iohid::spawn_caps_hid_monitor(caps_down.clone());
        // Physical Caps-LED writer (best-effort, self-retrying; falls back to the
        // lock-state write alone while it isn't open). See `led::RetryingCapsLed`.
        let led = led::RetryingCapsLed::new();
        // Does NOT own the Caps key here — the engine calls `acquire_caps_key` itself
        // right after construction, only if caps dictation starts enabled (see
        // `Engine::assemble`), so a `caps_enabled=false` startup never remaps the key at
        // all instead of remapping-then-immediately-suppressing.
        Ok(Self {
            caps,
            led,
            source,
            caps_down,
            owns_key: Cell::new(false),
            extra_terminals: RefCell::new(Vec::new()),
        })
    }
}

impl Drop for MacOsPlatform {
    fn drop(&mut self) {
        // Hand the Caps key back to the OS on clean shutdown, but only if we actually
        // took it (via the guarded `release_caps_key`, not the raw `capskey` function —
        // see `owns_key`'s doc for why an unconditional call here is unsafe: a session
        // that never called `acquire_caps_key` must not wipe a user's own unrelated
        // hidutil remap). (A hard SIGKILL skips this — the remap is per-login and is
        // cleared on the next clean run / logout / reboot.)
        self.release_caps_key();
        // Stop the dedicated CFRunLoop thread + release the IOHIDManager grab opened by
        // `spawn_caps_hid_monitor` in `Self::new` — otherwise every engine stop+start cycle
        // (this struct is reconstructed fresh per `engine_run` call, not reused) permanently
        // orphans one more thread and HID grab. `plat` is an `Rc<MacOsPlatform>` local to
        // `engine_run`'s stack (see `dontspeakd::boot::engine_run`), so this fires
        // automatically the moment the engine thread's function returns — no explicit
        // shutdown call site needed.
        iohid::stop_caps_hid_monitor();
    }
}

impl KeyInjector for MacOsPlatform {
    /// Tap the dictation chord ONCE via CGEvent through the `HIDSystemState` source. Two
    /// macOS specifics make this native impl (not a generic input lib) necessary:
    ///
    /// 1. It posts through the SAME source the Caps-Lock LED is read from — a key on a
    ///    different source desyncs the LED the recording edge-detector follows (a spurious
    ///    `stop` ~120ms after every start).
    /// 2. Each modifier is carried as a FLAG on the base-key event (exactly how a real
    ///    Ctrl+G arrives), not as a separate Control key press — so Claude Code's
    ///    Kitty-protocol parser sees the same thing as a hardware keypress.
    ///
    /// A short down→up hold gives Claude Code's (JS) event loop time to register the press.
    fn tap_key(&self, chord: &KeyChord) {
        let Some(keycode) = mac_keycode(&chord.base) else {
            crate::warn_unsupported_dictation_key(&chord.base);
            return;
        };
        let mut flags = CGEventFlags::empty();
        if chord.ctrl {
            flags |= CGEventFlags::CGEventFlagControl;
        }
        if chord.shift {
            flags |= CGEventFlags::CGEventFlagShift;
        }
        if chord.alt {
            flags |= CGEventFlags::CGEventFlagAlternate;
        }
        if chord.cmd {
            flags |= CGEventFlags::CGEventFlagCommand;
        }
        if let Ok(down) = CGEvent::new_keyboard_event(self.source.clone(), keycode, true) {
            if !flags.is_empty() {
                down.set_flags(flags);
            }
            down.post(CGEventTapLocation::Session);
        }
        // ~24ms hold: a real tap isn't instantaneous, and Claude Code's event loop needs a
        // beat to see the press before the release (an instant down+up was getting missed).
        std::thread::sleep(std::time::Duration::from_millis(24));
        if let Ok(up) = CGEvent::new_keyboard_event(self.source.clone(), keycode, false) {
            if !flags.is_empty() {
                up.set_flags(flags);
            }
            up.post(CGEventTapLocation::Session);
        }
    }
    /// Deliver `text` (a transcript) to the focused app via clipboard-paste (§C.3): set
    /// the clipboard, synth Cmd+V through the same source, then restore the clipboard. The
    /// caller (ds-stt) gates this on `is_terminal_frontmost()`. Atomic paste beats per-char
    /// typing for a multi-word transcript in a terminal. Fail-quiet.
    fn type_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Ok(mut cb) = arboard::Clipboard::new() else {
            return;
        };
        // Snapshot the user's clipboard text to RESTORE after the paste (None ⇒ non-text/
        // empty: clear what we put there rather than restore).
        let prev = cb.get_text().ok();
        if cb.set_text(text.to_string()).is_err() {
            return;
        }
        // Cmd+V with the SAME ~24ms down→up hold as `tap_key`: an instant down+up was
        // being missed by the target app's event loop, so the paste landed only
        // intermittently (and the auto-submit Return below then submitted nothing).
        if let Ok(down) = CGEvent::new_keyboard_event(self.source.clone(), KEY_V, true) {
            down.set_flags(CGEventFlags::CGEventFlagCommand);
            down.post(CGEventTapLocation::Session);
        }
        std::thread::sleep(std::time::Duration::from_millis(24));
        if let Ok(up) = CGEvent::new_keyboard_event(self.source.clone(), KEY_V, false) {
            up.set_flags(CGEventFlags::CGEventFlagCommand);
            up.post(CGEventTapLocation::Session);
        }
        // Restore the user's clipboard off-thread once the async Cmd+V has read ours.
        crate::restore_clipboard_after_paste(prev);
    }

    /// Tap Return once (no modifiers) — the always-listening loop's auto-submit. Same
    /// source/tap as the dictation key; the caller gates on terminal focus.
    fn press_enter(&self) {
        // Same ~24ms hold as `tap_key`/`type_text`: an instant down+up Return is liable to
        // be dropped by the target app's event loop, which would skip the auto-submit.
        if let Ok(down) = CGEvent::new_keyboard_event(self.source.clone(), KEY_RETURN, true) {
            down.post(CGEventTapLocation::Session);
        }
        std::thread::sleep(std::time::Duration::from_millis(24));
        if let Ok(up) = CGEvent::new_keyboard_event(self.source.clone(), KEY_RETURN, false) {
            up.post(CGEventTapLocation::Session);
        }
    }
}

/// Is `bid` (a macOS bundle identifier) one of the shared table's known terminal
/// identifiers (`ds_platform::KNOWN_TERMINALS`), OR one of the user's config.toml
/// `extra_terminals` entries? Replaces the old hand-maintained `TERM_BUNDLES` array.
/// `extra` entries are matched case-insensitively (a user may type any casing), unlike
/// the built-in table's exact-match literals.
fn is_known_terminal_bundle(bid: &str, extra: &[String]) -> bool {
    KNOWN_TERMINALS.iter().any(|t| t.macos_bundle == Some(bid))
        || extra.iter().any(|e| e.eq_ignore_ascii_case(bid))
}

impl FrontmostWindow for MacOsPlatform {
    fn is_terminal_frontmost(&self) -> bool {
        // THREAD SAFETY (reviewed 2026-06-20, macOS 14/15 Apple Silicon):
        // `-[NSWorkspace frontmostApplication]` is read off the engine's single
        // poll thread, NOT the main thread. This is intentional and safe:
        //   * `+[NSWorkspace sharedWorkspace]` returns a process-wide singleton;
        //     `frontmostApplication` reads a value kept current by the workspace
        //     notification machinery (an NSRunningApplication snapshot). It does
        //     not touch per-thread UI/AppKit drawing state, so there is no
        //     main-thread affinity to violate here. The original Swift daemon
        //     polled the same API off-main in shipping use without crashes or
        //     data races.
        //   * We deliberately do NOT dispatch_sync to the main queue: this binary
        //     has no CFRunLoop servicing the main dispatch queue (the main thread
        //     is the poll loop, sleeping between ticks), so a main-queue
        //     dispatch_sync would DEADLOCK. Off-main read is the correct choice.
        // If a future macOS makes this API main-thread-only, revisit by moving
        // the engine onto a CFRunLoop and querying via the main queue.
        // (objc2 0.6 exposes these AppKit getters as safe, so no `unsafe` here.)
        let ws = NSWorkspace::sharedWorkspace();
        let Some(app) = ws.frontmostApplication() else {
            return false;
        };
        match app.bundleIdentifier() {
            Some(bid) => {
                let s = bid.to_string();
                is_known_terminal_bundle(&s, &self.extra_terminals.borrow())
            }
            None => false,
        }
    }

    fn frontmost_app_name(&self) -> Option<String> {
        // Same off-main NSWorkspace read as `is_terminal_frontmost` (see the thread-
        // safety note there): the localized name of the app currently frontmost,
        // captured when dictation starts to label the confirm panel's paste target.
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        app.localizedName().map(|n| n.to_string())
    }

    fn has_paste_target(&self) -> bool {
        // Accessibility focused-element probe (see `iokit::focused_element_accepts_paste`):
        // is an editable field focused that would accept a synthetic Cmd+V right now?
        // Read off the engine poll thread; the AX call is a synchronous in-process
        // query with no main-thread affinity. Needs the Accessibility grant we already
        // hold for CGEventPost.
        iokit::focused_element_accepts_paste()
    }

    fn set_extra_terminals(&self, extra: Vec<String>) {
        *self.extra_terminals.borrow_mut() = extra;
    }

    // Deliberately no `set_extra_custom_text_editors` override — no macOS
    // `CUSTOM_TEXT_EXES`-equivalent exists yet (see GitHub issue #15), so the trait's
    // no-op default stands; the field is accepted but ignored here.
}

impl CapsKeyMonitor for MacOsPlatform {
    fn is_caps_physically_down(&self) -> bool {
        self.caps_down.load(Ordering::Relaxed)
    }
    fn caps_monitor_stuck(&self) -> bool {
        // Either of the two independent `IOHIDManagerOpen` call sites (the caps-HID
        // monitor and the LED writer) can hit the exact same stale-grant gotcha —
        // either one being stuck is reason enough to relaunch.
        iohid::is_caps_hid_stuck() || self.led.is_stuck()
    }
    fn caps_monitor_stuck_detail(&self) -> Option<&'static str> {
        match (iohid::is_caps_hid_stuck(), self.led.is_stuck()) {
            (true, true) => Some("the caps-HID monitor AND the Caps-LED writer"),
            (true, false) => Some("the caps-HID monitor"),
            (false, true) => Some("the Caps-LED writer"),
            (false, false) => None,
        }
    }
    fn set_caps_lock(&self, on: bool) {
        // Two writes, both targeting `on`: the LOGICAL lock (so a physical caps toggle
        // can't leave capitals stuck on) AND the PHYSICAL LED directly (reliable on
        // external/Bluetooth keyboards, where the lock-coupled write alone left the
        // light stuck — e.g. a tap that cancels playback). They agree, so no fighting.
        self.caps.set_caps_lock(on);
        self.led.set(on);
    }

    fn acquire_caps_key(&self) {
        // OWN the Caps key: remap it away from caps-lock at the HID driver level so a press
        // never enables capitals (the macOS equivalent of the Windows key suppression). The
        // physical key is still detected by the monitor spawned in `new`; the LED is ours
        // to drive.
        capskey::own_caps_key();
        self.owns_key.set(true);
        // Normalize on acquire: if caps lock was ON, the user can no longer toggle it off
        // (the key is remapped), so clear the logical lock and the indicator LED now.
        self.set_caps_lock(false);
    }

    fn release_caps_key(&self) {
        // Only if WE actually own it — `capskey::release_caps_key()` clears hidutil's
        // ENTIRE UserKeyMapping unconditionally (unlike the Linux port's marker-gated
        // release), so calling it when we never acquired (e.g. `caps_enabled` was false
        // all session) would wipe a user's own unrelated hidutil remap. See `owns_key`'s
        // doc on why an in-process flag is enough here (no cross-restart persistence
        // needed, unlike Linux's GNOME/KDE settings).
        if !self.owns_key.replace(false) {
            return;
        }
        capskey::release_caps_key();
    }
}

impl Platform for MacOsPlatform {
    fn preflight(&self) -> Result<(), PreflightError> {
        // SILENT, repeatable trust probe — the caps re-probe loop calls this on a
        // timer, so it must NOT prompt. The one-time prompt that registers DontSpeak
        // in the Accessibility list lives in `request_permissions` below.
        if iokit::ax_is_process_trusted() {
            Ok(())
        } else {
            Err(PreflightError(
                "not trusted for Accessibility — CGEventPost will silently fail. \
                 Grant this binary in System Settings > Privacy & Security > \
                 Accessibility, then reload the LaunchAgent."
                    .into(),
            ))
        }
    }

    fn request_permissions(&self) {
        // PROMPTING trust check (startup, once): registers DontSpeak in the
        // Accessibility list AND shows the one-time grant dialog, so a fresh install
        // gives the user a row to toggle instead of forcing a manual "+ add app".
        // We can't defer this to the first Caps-Lock press: the caps key is read via
        // IOHID, which ITSELF needs this grant (kIOReturnNotPermitted otherwise), so
        // an untrusted process never sees the press. We ignore the returned state —
        // preflight()/the re-probe loop own the live gate; this call only surfaces
        // the dialog + list row.
        let _ = iokit::ax_prompt_for_trust();
    }
}

// ── Microphone-in-use probe (TTS feedback gate) ──────────────────────────────
//
// macOS impl of the lib.rs probe: CoreAudio
// `kAudioDevicePropertyDeviceIsRunningSomewhere` on the default input device.

/// Returns true if the system's default microphone is currently capturing.
pub(crate) fn is_mic_active() -> bool {
    use std::os::raw::c_void;
    use std::ptr::NonNull;

    // Bindings + property-selector constants from objc2-core-audio (replaces a
    // hand-declared extern + FourCharCodes that had a latent selector typo). Same
    // selectors as <CoreAudio/AudioHardware.h>.
    use objc2_core_audio::{
        AudioObjectGetPropertyData, AudioObjectPropertyAddress,
        kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    };

    // SAFETY: both AudioObjectGetPropertyData calls satisfy the CoreAudio contract: a live
    // stack property address, null in-qualifier (with size 0), and `size`/data out-params
    // pointing at live stack u32s with the size initialized to their byte count — the API
    // writes at most that many bytes during the call. `NonNull::new(..).unwrap()` cannot
    // fail on addresses of stack locals.
    unsafe {
        // 1. Resolve the default input device.
        let dev_addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut device_id: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let rc = AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&dev_addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut device_id as *mut u32 as *mut c_void).unwrap(),
        );
        if rc != 0 || device_id == 0 {
            return false;
        }

        // 2. Is that device capturing somewhere right now?
        let run_addr = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut running: u32 = 0;
        let mut size2 = std::mem::size_of::<u32>() as u32;
        let rc2 = AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&run_addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size2),
            NonNull::new(&mut running as *mut u32 as *mut c_void).unwrap(),
        );
        rc2 == 0 && running != 0
    }
}

#[cfg(test)]
mod known_terminal_table {
    use super::*;

    /// The exact literal `TERM_BUNDLES` this crate carried before `KNOWN_TERMINALS`
    /// (ds-platform/src/lib.rs) replaced it — pinned here so a future edit to the
    /// shared table can't silently drop (or duplicate away) a macOS entry.
    const OLD_TERM_BUNDLES: &[&str] = &[
        "com.googlecode.iterm2",
        "com.apple.Terminal",
        "com.mitchellh.ghostty",
    ];

    #[test]
    fn matches_old_term_bundles_exactly() {
        let entries: Vec<&str> = KNOWN_TERMINALS
            .iter()
            .filter_map(|t| t.macos_bundle)
            .collect();
        let derived: std::collections::BTreeSet<&str> = entries.iter().copied().collect();
        let old: std::collections::BTreeSet<&str> = OLD_TERM_BUNDLES.iter().copied().collect();
        assert_eq!(
            derived, old,
            "KNOWN_TERMINALS' macos_bundle entries drifted from the pre-refactor TERM_BUNDLES list"
        );
        assert_eq!(
            entries.len(),
            derived.len(),
            "a macos_bundle value is duplicated across two KNOWN_TERMINALS rows"
        );
    }
}

#[cfg(test)]
mod extra_paste_targets {
    use super::*;

    #[test]
    fn is_known_terminal_bundle_matches_extra_case_insensitively() {
        let extra = vec!["com.example.myterm".to_string()];
        assert!(is_known_terminal_bundle("com.example.myterm", &extra));
        assert!(is_known_terminal_bundle("COM.EXAMPLE.MYTERM", &extra));
        assert!(!is_known_terminal_bundle("com.example.other", &extra));
        // Empty extra behaves exactly as before the signature change (regression guard).
        assert!(is_known_terminal_bundle("com.apple.Terminal", &[]));
        assert!(!is_known_terminal_bundle("com.example.notaterm", &[]));
    }
}
