//! Direct keyboard Caps-Lock LED writer via the HID Manager — the robust,
//! external-keyboard-safe way to drive the LED as the dictation indicator.
//!
//! Why this exists (the gap it closes): the engine drives the Caps LED as a pure
//! dictation indicator. The lock-coupled `IOHIDSetModifierLockState` write in
//! `iokit.rs` does NOT reliably reach EXTERNAL / Bluetooth keyboards — the same
//! blind spot that forces `iohid.rs` to read the physical key instead of the lock
//! state. So a press that should leave the LED OFF (e.g. a tap that cancels TTS
//! playback while idle) left the light stuck ON, whereas the key-owning Windows
//! port never lights it. Setting the device's `kHIDPage_LEDs` / Caps-Lock element
//! directly drives the PHYSICAL LED on every keyboard, decoupled from the logical
//! caps state (verified to NOT change the caps lock state) — the macOS analogue of
//! Linux's `EV_LED`/`LED_CAPSL` and the Windows `IOCTL_KEYBOARD_SET_INDICATORS`
//! path. `iokit::CapsReader::set_caps_lock` keeps driving the LOGICAL lock (so a
//! physical toggle can't leave capitals stuck on); this adds the physical-LED half.
//!
//! Uses the MODERN HID Manager API: the legacy `IOHIDDeviceInterface122` plug-in
//! path breaks on macOS 14+. Match keyboards, copy the (manager-opened) devices,
//! find each one's Caps-Lock LED element, and `IOHIDDeviceSetValue` an integer
//! on/off. Output `SetValue` is synchronous, so no run-loop scheduling is needed.
//! Symbols come from IOKit / CoreFoundation (linked in build.rs).
//!
//! UNCOMPILED on the non-macOS build host; verify on a Mac.

use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::ptr;

// IOKit / CoreFoundation opaque + scalar typedefs.
type IoHidManagerRef = *mut c_void;
type IoHidDeviceRef = *mut c_void;
type IoHidElementRef = *mut c_void;
type IoHidValueRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFArrayRef = *const c_void;
type CFSetRef = *const c_void;
type CFIndex = isize;
type IoReturn = c_int;
type IoOptionBits = c_uint;
type Boolean = u8;

const KIO_HID_OPTIONS_TYPE_NONE: IoOptionBits = 0;
const KIO_RETURN_SUCCESS: IoReturn = 0;

// HID usage pages / usages (from the HID usage tables).
const K_HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const K_HID_USAGE_GD_KEYBOARD: u32 = 0x06;
const K_HID_PAGE_LEDS: u32 = 0x08;
const K_HID_USAGE_LED_CAPSLOCK: u32 = 0x02;

unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: IoOptionBits) -> IoHidManagerRef;
    // NULL matching = match ALL devices; we filter to keyboards (IOHIDDeviceConformsTo)
    // and to the Caps LED element (by usage) below, so no CFDictionary is built.
    fn IOHIDManagerSetDeviceMatching(manager: IoHidManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerOpen(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    fn IOHIDManagerClose(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    fn IOHIDManagerCopyDevices(manager: IoHidManagerRef) -> CFSetRef;
    fn IOHIDDeviceConformsTo(device: IoHidDeviceRef, usage_page: u32, usage: u32) -> Boolean;
    fn IOHIDDeviceCopyMatchingElements(
        device: IoHidDeviceRef,
        matching: CFDictionaryRef,
        options: IoOptionBits,
    ) -> CFArrayRef;
    fn IOHIDElementGetUsagePage(element: IoHidElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IoHidElementRef) -> u32;
    fn IOHIDValueCreateWithIntegerValue(
        allocator: CFAllocatorRef,
        element: IoHidElementRef,
        timestamp: u64,
        value: CFIndex,
    ) -> IoHidValueRef;
    // Output `SetValue` needs the device OPEN — guaranteed here by IOHIDManagerOpen,
    // which opens every device the manager owns (else SetValue → kIOReturnNotOpen).
    fn IOHIDDeviceSetValue(
        device: IoHidDeviceRef,
        element: IoHidElementRef,
        value: IoHidValueRef,
    ) -> IoReturn;

    fn CFSetGetCount(set: CFSetRef) -> CFIndex;
    fn CFSetGetValues(set: CFSetRef, values: *mut *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Owns an open HID Manager (matching all devices) used to drive the physical
/// Caps-Lock LED on every keyboard.
pub struct CapsLed {
    manager: IoHidManagerRef,
}

// Touched only from the engine's single poll thread — same contract as
// `iokit::CapsReader` (whose connection carries the identical `unsafe impl Send`).
unsafe impl Send for CapsLed {}

impl CapsLed {
    /// Create + open the manager. `None` if it can't open (e.g. permissions) — the
    /// caller then falls back to the lock-state LED write alone.
    pub fn open() -> Option<Self> {
        unsafe {
            let manager = IOHIDManagerCreate(ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
            if manager.is_null() {
                return None;
            }
            IOHIDManagerSetDeviceMatching(manager, ptr::null());
            if IOHIDManagerOpen(manager, KIO_HID_OPTIONS_TYPE_NONE) != KIO_RETURN_SUCCESS {
                CFRelease(manager);
                return None;
            }
            Some(CapsLed { manager })
        }
    }

    /// Drive the Caps-Lock LED on every keyboard to `on`, WITHOUT changing the
    /// logical caps state. Best-effort: re-enumerates devices each call (so a
    /// hot-plugged keyboard is covered) and ignores per-device failures.
    pub fn set(&self, on: bool) {
        unsafe {
            let devices = IOHIDManagerCopyDevices(self.manager);
            if devices.is_null() {
                return;
            }
            let count = CFSetGetCount(devices);
            if count > 0 {
                let mut refs: Vec<*const c_void> = vec![ptr::null(); count as usize];
                CFSetGetValues(devices, refs.as_mut_ptr());
                for &d in &refs {
                    let device = d as IoHidDeviceRef;
                    if device.is_null()
                        || IOHIDDeviceConformsTo(
                            device,
                            K_HID_PAGE_GENERIC_DESKTOP,
                            K_HID_USAGE_GD_KEYBOARD,
                        ) == 0
                    {
                        continue;
                    }
                    set_caps_led_on_device(device, on);
                }
            }
            // `IOHIDManagerCopyDevices` returns a +1 CFSet; the device refs inside are
            // borrowed (do NOT release them individually).
            CFRelease(devices);
        }
    }
}

/// Find the device's Caps-Lock LED element and set it. `device` is borrowed.
unsafe fn set_caps_led_on_device(device: IoHidDeviceRef, on: bool) {
    unsafe {
        // NULL matching = all elements; filter to the Caps LED by usage page/usage.
        let elements =
            IOHIDDeviceCopyMatchingElements(device, ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
        if elements.is_null() {
            return;
        }
        let n = CFArrayGetCount(elements);
        for i in 0..n {
            let element = CFArrayGetValueAtIndex(elements, i) as IoHidElementRef;
            if element.is_null() {
                continue;
            }
            if IOHIDElementGetUsagePage(element) == K_HID_PAGE_LEDS
                && IOHIDElementGetUsage(element) == K_HID_USAGE_LED_CAPSLOCK
            {
                // +1 value; release after the set.
                let value =
                    IOHIDValueCreateWithIntegerValue(ptr::null(), element, 0, on as CFIndex);
                if !value.is_null() {
                    let _ = IOHIDDeviceSetValue(device, element, value);
                    CFRelease(value);
                }
                // One Caps-LED element per keyboard — stop after the first.
                break;
            }
        }
        // `IOHIDDeviceCopyMatchingElements` returns a +1 CFArray; elements borrowed.
        CFRelease(elements);
    }
}

impl Drop for CapsLed {
    fn drop(&mut self) {
        unsafe {
            if !self.manager.is_null() {
                IOHIDManagerClose(self.manager, KIO_HID_OPTIONS_TYPE_NONE);
                CFRelease(self.manager);
                self.manager = ptr::null_mut();
            }
        }
    }
}
