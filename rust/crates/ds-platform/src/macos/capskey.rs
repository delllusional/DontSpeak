//! "Own the Caps key" on macOS — neutralize its caps-lock TOGGLE at the HID driver
//! level so a physical press never enables capitals. The macOS equivalent of the
//! Windows `WH_KEYBOARD_LL` suppression: DontSpeak fully owns Caps as a dictation key.
//!
//! Why a driver-level remap (not a CGEventTap): caps lock is special — consuming the
//! `kCGEventFlagsChanged` event in a tap does NOT reliably stop the OS toggle, which is
//! exactly why the established tools (Karabiner, Hyperkey, CapsLockSwitcher) remap it at
//! the HID layer instead. `hidutil property --set UserKeyMapping` is Apple's own public,
//! no-sudo way to do that: we remap Caps Lock (0x700000039) → **No Action** (0x700000000),
//! so a press toggles nothing and delivers NO key to the focused app.
//!
//! Why the null usage and not an "inert" key like F18: a remap SUBSTITUTES, it doesn't
//! consume — an F18 dst still reaches the frontmost app, and terminals encode F18 as
//! `ESC[32~`, so every Caps press typed stray `~`/`2~` into any focused shell. The bare
//! `0x700000000` dst ("No Action") suppresses that entirely. Caveat: TN2450 only documents
//! dst values as `0x700000000 | usage` — the bare prefix as a disable is a long-standing
//! community convention (the standard "disable caps lock" recipe), not an Apple-documented
//! contract; verified working on macOS 14–26 as of 2026-07.
//!
//! Detection is UNAFFECTED: `iohid::spawn_caps_hid_monitor` reads the raw device input
//! element (usage 0x39) straight off the keyboard, below this system-level remap, so the
//! dst never mattered to it. The LED is then driven entirely by us (`led::CapsLed`) as the
//! dictation indicator, since the OS no longer toggles it.
//!
//! Lifecycle: applied in `MacOsPlatform::new`, restored in its `Drop`. A `hidutil` mapping
//! is per-login and does NOT survive logout/reboot, and we clear-then-apply on start, so
//! the only residue window is a HARD kill (SIGKILL) with no relaunch — caps stays remapped
//! until the next clean run, a logout, or a reboot. Documented; acceptable (same model as
//! Karabiner-style tools).

use std::process::Command;

/// hidutil HID-usage IDs: (page 0x07 << 32) | usage. Caps Lock = 0x39; the bare page
/// prefix with no usage bits is the conventional "No Action" (key disabled) destination.
const SRC_CAPS_LOCK: &str = "0x700000039";
const DST_NO_ACTION: &str = "0x700000000";

/// `hidutil property --set '{"UserKeyMapping":[...]}'`. Returns whether it succeeded.
/// hidutil's parser accepts the `0x…` hex literals inside the (otherwise-JSON) value.
fn set_user_key_mapping(pairs: &str) -> bool {
    let value = format!("{{\"UserKeyMapping\":[{pairs}]}}");
    match Command::new("hidutil")
        .args(["property", "--set", &value])
        .status()
    {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("[dontspeak] hidutil failed ({e}); Caps will still toggle capitals");
            false
        }
    }
}

/// Take ownership of the Caps key: remap Caps Lock → No Action so it never toggles caps
/// lock and never types anything. Best-effort — on failure the key falls back to normal
/// caps-lock behavior.
pub fn own_caps_key() {
    let pair = format!(
        "{{\"HIDKeyboardModifierMappingSrc\":{SRC_CAPS_LOCK},\"HIDKeyboardModifierMappingDst\":{DST_NO_ACTION}}}"
    );
    if set_user_key_mapping(&pair) {
        eprintln!("[dontspeak] owning Caps key (remapped Caps Lock → No Action; no caps toggle)");
    }
}

/// Release the Caps key back to the OS: clear our remap (empty UserKeyMapping).
pub fn release_caps_key() {
    set_user_key_mapping("");
}
