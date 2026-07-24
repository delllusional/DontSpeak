// Diagnostic sink for libdontspeak_sys: Rust registers one C callback so shim diagnostics
// land in DontSpeak's own log instead of the helper's stderr. See dontspeak_shim.h.
//
// Every module-visible name here carries the `Sys`/`sys` prefix. The three shim modules are
// statically linked into one XCTest bundle, and Swift cannot disambiguate identically named
// module-visible declarations across `@testable import`s, so `sys` / `mlx` / `fluid` keep
// disjoint top-level names (`SysLogSink` vs `MlxLogSink` vs `FluidLogSink`, and so on).

import Foundation

public typealias SysLogCb = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Mirror of dontspeak_shim.h's DS_SHIM_LOG_* -- four fixed integers, hand-kept in sync with
/// that header and ds-model's `forward`. No build check covers this; the header is canonical.
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

    /// Stderr-fallback count. Lets LogSinkTests prove the sink took over without redirecting
    /// the shared XCTest process's fd 2.
    func fallbackCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return fallbacks
    }

    /// Host log when a sink is registered; stderr otherwise (smoke binary, XCTest, and the
    /// window before `open()` registers one). The pointer is copied out and the lock released
    /// BEFORE the call -- the sink must never run under this lock, so a sink may legitimately
    /// fire just after being deregistered (benign: the Rust thunk is 'static in the Rust image).
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
