// Diagnostic sink for libdontspeak_sys: Rust registers one C callback so shim
// diagnostics land in DontSpeak's log. See dontspeak_shim.h.
//
// Sys/sys-prefixed names stay disjoint from mlx/fluid (one XCTest bundle,
// @testable imports cannot disambiguate shared top-level names).

import Foundation

public typealias SysLogCb = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Mirror of dontspeak_shim.h DS_SHIM_LOG_*; header is canonical (also mirrored in ds-model `forward`).
enum SysLogLevel {
    static let debug: Int32 = 0
    static let info: Int32 = 1
    static let warn: Int32 = 2
    static let error: Int32 = 3
}

final class SysLogSink: @unchecked Sendable {
    private let lock = NSLock()
    private var cb: SysLogCb?
    private var fallbacks = 0

    func set(_ cb: SysLogCb?) {
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

let sysLogSink = SysLogSink()

@_cdecl("ds_sys_set_log_cb")
public func ds_sys_set_log_cb(_ cb: SysLogCb?) { sysLogSink.set(cb) }

func sysLogErr(_ s: String) { sysLogSink.emit(SysLogLevel.error, s) }
func sysLogWarn(_ s: String) { sysLogSink.emit(SysLogLevel.warn, s) }
