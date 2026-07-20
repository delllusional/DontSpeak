//! Own Caps on macOS: HID remap Caps → No Action so presses never toggle capitals
//! (Windows WH_KEYBOARD_LL analogue). CGEventTap can't reliably stop the OS Caps toggle.
//!
//! `hidutil` UserKeyMapping: 0x700000039 → 0x700000000 (No Action, not F18 — F18 leaks
//! escape sequences into shells). TN2450 only documents `| usage` dests; bare prefix is
//! community convention, verified macOS 14–26. Physical hold still via raw HID (`iohid`);
//! LED driven by us. Applied in platform new, restored on Drop; residue only after SIGKILL.

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
            log::warn!(
                target: "platform",
                "hidutil failed ({e}); Caps will still toggle capitals"
            );
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
        log::info!(
            target: "platform",
            "owning Caps key (remapped Caps Lock → No Action; no caps toggle)"
        );
    }
}

/// Release the Caps key back to the OS: clear our remap (empty UserKeyMapping).
pub fn release_caps_key() {
    set_user_key_mapping("");
}
