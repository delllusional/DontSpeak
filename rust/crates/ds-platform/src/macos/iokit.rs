//! Thin IOKit FFI for the Caps Lock LED WRITE + Accessibility-trust check.
//!
//! Opens an IOHIDSystem param connection (mirrors the Swift daemon):
//!   service = IOServiceGetMatchingService(kIOMainPortDefault,
//!                                         IOServiceMatching("IOHIDSystem"))
//!   IOServiceOpen(service, mach_task_self, kIOHIDParamConnectType, &connect)
//!   IOHIDSetModifierLockState(connect, kIOHIDCapsLockState, on)   // §F LED reset
//!
//! The lock-state READ (`IOHIDGetModifierLockState`) is deliberately NOT used:
//! it never tracks toggles on this host's external keyboard, so the HOLD signal
//! comes from the physical-HID monitor in `iohid.rs` instead.
//!
//! These symbols come from the IOKit framework (linked in build.rs).
//! `AXIsProcessTrusted` comes from ApplicationServices (also linked there).

use std::ffi::c_void;
use std::os::raw::{c_char, c_int, c_uint};

// IOKit opaque/scalar typedefs (all are mach port-ish u32 on Darwin).
type KernReturn = c_int;
type MachPort = c_uint;
type IoService = c_uint;
type IoConnect = c_uint;
type IoObject = c_uint;
type CFDictionaryRef = *const c_void;
type Boolean = u8;

const KERN_SUCCESS: KernReturn = 0;
// kIOHIDParamConnectType == 1; kIOHIDCapsLockState == 0 (from IOKit headers).
const KIO_HID_PARAM_CONNECT_TYPE: c_uint = 1;
const KIO_HID_CAPS_LOCK_STATE: c_int = 0;

unsafe extern "C" {
    // kIOMainPortDefault is the value 0 (formerly kIOMasterPortDefault). We pass
    // 0 directly rather than importing the symbol (kept stable across SDKs).
    fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: CFDictionaryRef) -> IoService;
    fn IOServiceOpen(
        service: IoService,
        owning_task: MachPort,
        type_: c_uint,
        connect: *mut IoConnect,
    ) -> KernReturn;
    fn IOServiceClose(connect: IoConnect) -> KernReturn;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    // Caps Lock LED/lock WRITE (the §F drift-recovery reset drives the LED off).
    // NOTE: the READ sibling `IOHIDGetModifierLockState` is intentionally absent —
    // the lock state never syncs to IOHIDSystem on this host's external keyboard,
    // so the HOLD signal comes from `iohid::spawn_caps_hid_monitor` (physical HID
    // key) instead, and only the LED WRITE remains useful here.
    fn IOHIDSetModifierLockState(connect: IoConnect, selector: c_int, state: Boolean)
    -> KernReturn;

    // The current task's mach port. `mach_task_self_` is a global; expose it.
    static mach_task_self_: MachPort;

    // ApplicationServices: read-only Accessibility-trust check (no prompt).
    fn AXIsProcessTrusted() -> Boolean;

    // ── Accessibility focused-element probe (paste-target detection) ──────────
    // ApplicationServices (HIServices). Used by `focused_element_accepts_paste`
    // to answer "would a paste land in an editable field right now?". Needs the
    // same Accessibility grant `AXIsProcessTrusted` checks (already required for
    // CGEventPost), so no new permission.
    fn AXUIElementCreateSystemWide() -> AxUiElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AxUiElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AxError;
    fn AXUIElementIsAttributeSettable(
        element: AxUiElementRef,
        attribute: CFStringRef,
        settable: *mut Boolean,
    ) -> AxError;

    // CoreFoundation: build the attribute-name CFStrings + release the +1 refs we
    // own (Create/Copy rule). Linked via build.rs.
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const c_void);
    // Compare two CFStrings (the focused element's AXRole vs. a known text role).
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> Boolean;
}

/// Whether `role` (a CFString from `kAXRoleAttribute`) is one of the editable
/// text-input roles a paste would land in. The canonical "is a text field focused"
/// test (Apple Accessibility): native text inputs report `AXTextField` / `AXTextArea`
/// (search/secure/combo are variants). Precise where the settable-`AXValue` heuristic
/// over-matches (e.g. sliders/steppers also report a settable value).
unsafe fn role_is_text_input(role: CFTypeRef) -> bool {
    const TEXT_ROLES: [&[u8]; 5] = [
        b"AXTextField\0",
        b"AXTextArea\0",
        b"AXComboBox\0",
        b"AXSearchField\0",
        b"AXSecureTextField\0",
    ];
    unsafe {
        for r in TEXT_ROLES {
            let cf = cfstr(r);
            if cf.is_null() {
                continue;
            }
            let eq = CFEqual(role, cf) != 0;
            CFRelease(cf);
            if eq {
                return true;
            }
        }
    }
    false
}

// AXUIElementRef / CFStringRef / CFTypeRef are all opaque CoreFoundation pointers.
type AxUiElementRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
// AXError is a signed enum; kAXErrorSuccess == 0.
type AxError = c_int;
const KAX_ERROR_SUCCESS: AxError = 0;
// kCFStringEncodingUTF8.
const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// Make an owned (+1) CFString from a NUL-terminated ASCII literal. Caller must
/// `CFRelease` it. Returns null on failure.
unsafe fn cfstr(name: &[u8]) -> CFStringRef {
    debug_assert_eq!(name.last(), Some(&0), "cfstr name must be NUL-terminated");
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            name.as_ptr() as *const c_char,
            KCF_STRING_ENCODING_UTF8,
        )
    }
}

/// Whether the system-wide FOCUSED accessibility element is an editable field that
/// would accept a paste right now — the macOS `paste_target_present` probe.
///
/// `AXUIElementCreateSystemWide` + `kAXFocusedUIElementAttribute` yields whatever
/// element currently has keyboard focus (nil/error ⇒ nothing focused ⇒ no paste
/// target). We accept it as a text input when EITHER its `kAXRoleAttribute` is a
/// text-input role (`AXTextField`/`AXTextArea`/… — the canonical Apple Accessibility
/// test; precise, so it excludes sliders/steppers) OR `kAXValueAttribute` is settable
/// (a fallback that also catches editable elements reporting a non-standard role, since
/// a text field/area exposes its contents through a settable value). The role check is
/// primary for correctness; the settable check widens coverage so a legit field rarely
/// reads as "no target" (which would show a spurious warning glow).
///
/// Conservative: ANY failure returns false (no target). Apps that don't expose AX focus
/// (some Electron/Java/custom-drawn UIs) read as "no target".
pub fn focused_element_accepts_paste() -> bool {
    unsafe {
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() {
            return false;
        }
        let focused_attr = cfstr(b"AXFocusedUIElement\0");
        let mut focused: CFTypeRef = std::ptr::null();
        let err = if focused_attr.is_null() {
            -1
        } else {
            AXUIElementCopyAttributeValue(sys, focused_attr, &mut focused)
        };
        CFRelease(sys);
        if !focused_attr.is_null() {
            CFRelease(focused_attr);
        }
        if err != KAX_ERROR_SUCCESS || focused.is_null() {
            return false;
        }

        // Primary: the focused element's role is a text-input role.
        let role_attr = cfstr(b"AXRole\0");
        let mut role_match = false;
        if !role_attr.is_null() {
            let mut role: CFTypeRef = std::ptr::null();
            let rerr = AXUIElementCopyAttributeValue(focused, role_attr, &mut role);
            CFRelease(role_attr);
            if rerr == KAX_ERROR_SUCCESS && !role.is_null() {
                role_match = role_is_text_input(role);
                CFRelease(role);
            }
        }

        // Fallback: a settable AXValue (editable contents) widens coverage.
        let settable_match = if role_match {
            true
        } else {
            let value_attr = cfstr(b"AXValue\0");
            let mut settable: Boolean = 0;
            let serr = if value_attr.is_null() {
                -1
            } else {
                AXUIElementIsAttributeSettable(focused, value_attr, &mut settable)
            };
            if !value_attr.is_null() {
                CFRelease(value_attr);
            }
            serr == KAX_ERROR_SUCCESS && settable != 0
        };

        // `focused` came back +1 from the Copy call — release our reference.
        CFRelease(focused);
        role_match || settable_match
    }
}

const K_IO_MAIN_PORT_DEFAULT: MachPort = 0;
const IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";

/// Owns the IOHIDSystem param connection used to drive the caps lock LED (§F).
pub struct CapsReader {
    connect: IoConnect,
    service: IoService,
}

// The connection is only touched from the engine's single poll thread.
unsafe impl Send for CapsReader {}

impl CapsReader {
    pub fn open() -> Option<Self> {
        unsafe {
            let matching = IOServiceMatching(IOHID_SYSTEM_CLASS.as_ptr() as *const c_char);
            if matching.is_null() {
                return None;
            }
            // REFERENCE TRANSFER: IOServiceGetMatchingService *consumes* (releases)
            // the CFDictionaryRef returned by IOServiceMatching — per Apple IOKit
            // docs, ownership of `matching` passes into the call. We must NOT
            // CFRelease it separately; doing so would be an over-release / double
            // free. Do not "fix" this by adding a release.
            let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
            if service == 0 {
                return None;
            }
            let mut connect: IoConnect = 0;
            let rc = IOServiceOpen(
                service,
                mach_task_self_,
                KIO_HID_PARAM_CONNECT_TYPE,
                &mut connect,
            );
            if rc != KERN_SUCCESS {
                IOObjectRelease(service);
                return None;
            }
            Some(CapsReader { connect, service })
        }
    }

    /// Force the Caps Lock LOGICAL lock state on the IOHIDSystem param connection
    /// (keeps a physical toggle from leaving capitals stuck on). On INTERNAL keyboards
    /// this also drives the LED, but it's unreliable on external/Bluetooth ones — so the
    /// PHYSICAL LED is driven directly in `super::led::CapsLed`. Best-effort (ignores
    /// the KernReturn).
    pub fn set_caps_lock(&self, on: bool) {
        unsafe {
            let _ = IOHIDSetModifierLockState(self.connect, KIO_HID_CAPS_LOCK_STATE, on as Boolean);
        }
    }
}

impl Drop for CapsReader {
    fn drop(&mut self) {
        unsafe {
            if self.connect != 0 {
                IOServiceClose(self.connect);
                self.connect = 0;
            }
            if self.service != 0 {
                IOObjectRelease(self.service);
                self.service = 0;
            }
        }
    }
}

/// `AXIsProcessTrusted()` — read-only Accessibility trust check (no prompt).
pub fn ax_is_process_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}
