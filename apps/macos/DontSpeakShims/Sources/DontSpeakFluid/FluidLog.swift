// Diagnostic sink for libdontspeak_fluid: Rust registers one C callback so shim
// diagnostics land in DontSpeak's log. See dontspeak_shim.h.
//
// FluidAudio AppLogger has no redirection hook (v0.15.5); only ds_fluid_*
// diagnostics route here. Fluid/fluid-prefixed names stay disjoint from sys/mlx
// (one XCTest bundle, @testable imports cannot disambiguate shared top-level names).

import Foundation

public typealias FluidLogCb = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Mirror of dontspeak_shim.h DS_SHIM_LOG_*; header is canonical (also mirrored in ds-model `forward`).
enum FluidLogLevel {
    static let debug: Int32 = 0
    static let info: Int32 = 1
    static let warn: Int32 = 2
    static let error: Int32 = 3
}

final class FluidLogSink: @unchecked Sendable {
    private let lock = NSLock()
    private var cb: FluidLogCb?
    private var fallbacks = 0

    func set(_ cb: FluidLogCb?) {
        lock.lock()
        defer { lock.unlock() }
        self.cb = cb
    }

    /// Stderr-fallback count for LogSinkTests (avoids redirecting XCTest fd 2).
    func fallbackCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return fallbacks
    }

    /// Registered sink, else stderr. Copies cb out and unlocks before invoke so the
    /// sink never runs under this lock (may fire just after deregister; Rust thunk is 'static).
    func emit(_ level: Int32, _ s: String) {
        lock.lock()
        let cb = self.cb
        if cb == nil { fallbacks += 1 }
        lock.unlock()
        guard let cb else {
            FileHandle.standardError.write(Data((s + "\n").utf8))
            return
        }
        s.withCString { cb(level, $0) }
    }
}

let fluidLogSink = FluidLogSink()

@_cdecl("ds_fluid_set_log_cb")
public func ds_fluid_set_log_cb(_ cb: FluidLogCb?) { fluidLogSink.set(cb) }

func fluidLogErr(_ s: String) { fluidLogSink.emit(FluidLogLevel.error, s) }
func fluidLogWarn(_ s: String) { fluidLogSink.emit(FluidLogLevel.warn, s) }
