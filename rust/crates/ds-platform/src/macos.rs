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
//!   objc2-app-kit, matched against [`crate::TERM_BUNDLES`].
//! * Preflight: `AXIsProcessTrusted()` (read-only, no prompt).

mod capskey;
mod iohid;
mod iokit;
mod led;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use objc2_app_kit::NSWorkspace;

use crate::{
    CapsKeyMonitor, FrontmostWindow, KeyBase, KeyChord, KeyInjector, Platform, PreflightError,
    TERM_BUNDLES,
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

pub struct MacPlatform {
    caps: iokit::CapsReader,
    /// Direct physical-LED writer (HID Manager). Drives the Caps-Lock LED on every
    /// keyboard, decoupled from the logical lock — the part `iokit`'s lock-coupled
    /// write can't reliably do on external/Bluetooth keyboards. `None` if the HID
    /// manager wouldn't open (then only the lock-state write drives the LED).
    led: Option<led::CapsLed>,
    /// The CGEventSource the dictation tap is posted through. MUST be `HIDSystemState` so a
    /// synthesized keypress can't clobber the Caps-Lock LED the engine drives as its recording
    /// indicator. (A key posted through a DIFFERENT source flips the Caps lock/LED → a spurious
    /// toggle ~120 ms after each start; this coupling is exactly why key injection here can't
    /// be generic.)
    source: CGEventSource,
    /// Physical Caps-key down state, published by the IOHIDManager monitor thread
    /// (`iohid::spawn_caps_hid_monitor`). Read synchronously by `read()` (HOLD
    /// trigger) and `caps_physically_down()` (§F long-press) from the engine's
    /// poll thread.
    caps_down: Arc<AtomicBool>,
}

impl MacPlatform {
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
        // Physical Caps-LED writer (best-effort; falls back to the lock-state write
        // alone if the HID manager can't open). See `led.rs`.
        let led = led::CapsLed::open();
        // OWN the Caps key: remap it away from caps-lock at the HID driver level so a press
        // never enables capitals (the macOS equivalent of the Windows key suppression). The
        // physical key is still detected by the monitor above; the LED is ours to drive.
        capskey::own_caps_key();
        // Normalize at startup: if caps lock was ON, the user can no longer toggle it off
        // (the key is remapped), so clear the logical lock and the indicator LED now.
        caps.set_caps_lock(false);
        if let Some(l) = &led {
            l.set(false);
        }
        Ok(Self {
            caps,
            led,
            source,
            caps_down,
        })
    }
}

impl Drop for MacPlatform {
    fn drop(&mut self) {
        // Hand the Caps key back to the OS on clean shutdown. (A hard SIGKILL skips this —
        // the remap is per-login and is cleared on the next clean run / logout / reboot.)
        capskey::release_caps_key();
    }
}

impl KeyInjector for MacPlatform {
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
    /// caller (ds-stt) gates this on `terminal_frontmost()`. Atomic paste beats per-char
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

impl FrontmostWindow for MacPlatform {
    fn terminal_frontmost(&self) -> bool {
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
                TERM_BUNDLES.contains(&s.as_str())
            }
            None => false,
        }
    }

    fn frontmost_app_name(&self) -> Option<String> {
        // Same off-main NSWorkspace read as `terminal_frontmost` (see the thread-
        // safety note there): the localized name of the app currently frontmost,
        // captured when dictation starts to label the confirm panel's paste target.
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        app.localizedName().map(|n| n.to_string())
    }

    fn paste_target_present(&self) -> bool {
        // Accessibility focused-element probe (see `iokit::focused_element_accepts_paste`):
        // is an editable field focused that would accept a synthetic Cmd+V right now?
        // Read off the engine poll thread; the AX call is a synchronous in-process
        // query with no main-thread affinity. Needs the Accessibility grant we already
        // hold for CGEventPost.
        iokit::focused_element_accepts_paste()
    }
}

impl CapsKeyMonitor for MacPlatform {
    fn caps_physically_down(&self) -> bool {
        self.caps_down.load(Ordering::Relaxed)
    }
    fn set_caps_lock(&self, on: bool) {
        // Two writes, both targeting `on`: the LOGICAL lock (so a physical caps toggle
        // can't leave capitals stuck on) AND the PHYSICAL LED directly (reliable on
        // external/Bluetooth keyboards, where the lock-coupled write alone left the
        // light stuck — e.g. a tap that cancels playback). They agree, so no fighting.
        self.caps.set_caps_lock(on);
        if let Some(led) = &self.led {
            led.set(on);
        }
    }
}

impl Platform for MacPlatform {
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
pub(crate) fn mic_active() -> bool {
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
