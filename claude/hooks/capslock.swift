// Set or toggle the hardware Caps Lock state (and its LED) via IOKit.
// Usage: capslock [on|off|toggle]   (default: toggle). Prints the new state.
// Reliable hardware-LED control via IOKit — remapping the Caps key elsewhere does
// not reliably drive the physical LED.
import Foundation
import IOKit
import IOKit.hidsystem

let arg = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "toggle"

let service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching(kIOHIDSystemClass))
guard service != 0 else { FileHandle.standardError.write("no IOHIDSystem\n".data(using: .utf8)!); exit(2) }

var connect: io_connect_t = 0
guard IOServiceOpen(service, mach_task_self_, UInt32(kIOHIDParamConnectType), &connect) == KERN_SUCCESS else {
    IOObjectRelease(service); FileHandle.standardError.write("IOServiceOpen failed\n".data(using: .utf8)!); exit(2)
}

var state = false
IOHIDGetModifierLockState(connect, Int32(kIOHIDCapsLockState), &state)

let newState: Bool
switch arg {
case "on":  newState = true
case "off": newState = false
default:    newState = !state
}
IOHIDSetModifierLockState(connect, Int32(kIOHIDCapsLockState), newState)

IOServiceClose(connect)
IOObjectRelease(service)
print(newState ? "on" : "off")
