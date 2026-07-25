// Diagnostic sink for libdontspeak_mlx: Rust registers one C callback so shim
// diagnostics land in DontSpeak's log. See dontspeak_shim.h.
//
// Keeps unprefixed logErr/logWarn; sys and fluid prefix theirs so the three
// XCTest-linked modules keep disjoint top-level names.

import Foundation

public typealias MlxLogCb = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void

/// Mirror of dontspeak_shim.h DS_SHIM_LOG_*; header is canonical (also mirrored in ds-model `forward`).
enum MlxLogLevel {
    static let debug: Int32 = 0
    static let info: Int32 = 1
    static let warn: Int32 = 2
    static let error: Int32 = 3
}

final class MlxLogSink: @unchecked Sendable {
    private let lock = NSLock()
    private var cb: MlxLogCb?
    private var fallbacks = 0

    func set(_ cb: MlxLogCb?) {
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

let mlxLogSink = MlxLogSink()

@_cdecl("ds_mlx_set_log_cb")
public func ds_mlx_set_log_cb(_ cb: MlxLogCb?) { mlxLogSink.set(cb) }

func logErr(_ s: String) { mlxLogSink.emit(MlxLogLevel.error, s) }
func logInfo(_ s: String) { mlxLogSink.emit(MlxLogLevel.info, s) }
func logWarn(_ s: String) { mlxLogSink.emit(MlxLogLevel.warn, s) }
