import XCTest

@testable import DontSpeakFluid
@testable import DontSpeakMLX
@testable import DontSpeakSys

// `@convention(c)` closures capture nothing — probes count via file-scope globals.
// Takeover cases are single-threaded; concurrency cases register no-op probes so they
// never touch these.
private nonisolated(unsafe) var sysHits = 0
private nonisolated(unsafe) var mlxHits = 0
private nonisolated(unsafe) var fluidHits = 0

private let sysProbe: SysLogCb = { _, _ in sysHits += 1 }
private let mlxProbe: MlxLogCb = { _, _ in mlxHits += 1 }
private let fluidProbe: FluidLogCb = { _, _ in fluidHits += 1 }

/// Per-family log sinks: registered cb takes every line; clear restores stderr fallback.
/// Observed via fallback counter (not fd 2 redirect, which can deadlock on a full pipe).
///
/// One file imports all three modules only because module-visible names are disjoint.
/// If that breaks, split into three files rather than reintroducing shared names.
final class LogSinkTests: XCTestCase {
    /// Sinks are process-global — a leak would change what SysAbiTests/FluidAbiTests log.
    override func tearDown() {
        ds_sys_set_log_cb(nil)
        ds_mlx_set_log_cb(nil)
        ds_fluid_set_log_cb(nil)
        super.tearDown()
    }

    func testSysSinkTakesOverFromStderr() {
        let before = sysLogSink.fallbackCount()
        sysHits = 0
        ds_sys_set_log_cb(sysProbe)
        sysLogSink.emit(SysLogLevel.error, "logsink probe")
        XCTAssertEqual(sysHits, 1)
        XCTAssertEqual(sysLogSink.fallbackCount(), before)
        ds_sys_set_log_cb(nil)
        sysLogSink.emit(SysLogLevel.error, "logsink probe (fallback)")
        XCTAssertEqual(sysHits, 1)
        XCTAssertEqual(sysLogSink.fallbackCount(), before + 1)
    }

    func testMlxSinkTakesOverFromStderr() {
        let before = mlxLogSink.fallbackCount()
        mlxHits = 0
        ds_mlx_set_log_cb(mlxProbe)
        mlxLogSink.emit(MlxLogLevel.error, "logsink probe")
        XCTAssertEqual(mlxHits, 1)
        XCTAssertEqual(mlxLogSink.fallbackCount(), before)
        ds_mlx_set_log_cb(nil)
        mlxLogSink.emit(MlxLogLevel.error, "logsink probe (fallback)")
        XCTAssertEqual(mlxHits, 1)
        XCTAssertEqual(mlxLogSink.fallbackCount(), before + 1)
    }

    func testFluidSinkTakesOverFromStderr() {
        let before = fluidLogSink.fallbackCount()
        fluidHits = 0
        ds_fluid_set_log_cb(fluidProbe)
        fluidLogSink.emit(FluidLogLevel.error, "logsink probe")
        XCTAssertEqual(fluidHits, 1)
        XCTAssertEqual(fluidLogSink.fallbackCount(), before)
        ds_fluid_set_log_cb(nil)
        fluidLogSink.emit(FluidLogLevel.error, "logsink probe (fallback)")
        XCTAssertEqual(fluidHits, 1)
        XCTAssertEqual(fluidLogSink.fallbackCount(), before + 1)
    }

    // Reentrancy: emit copies the cb under the lock and unlocks before invoke, so a sink
    // may fire just after deregister (benign; Rust thunk is 'static). Must not deadlock.
    // Probes are no-ops — concurrent @convention(c) on an unsync counter would race.

    func testSysSinkSurvivesConcurrentRegistrationAndEmit() {
        let probe: SysLogCb = { _, _ in }
        DispatchQueue.concurrentPerform(iterations: 1000) { index in
            switch index % 3 {
            case 0: ds_sys_set_log_cb(probe)
            case 1: ds_sys_set_log_cb(nil)
            default: sysLogSink.emit(SysLogLevel.warn, "concurrent probe")
            }
        }
    }

    func testMlxSinkSurvivesConcurrentRegistrationAndEmit() {
        let probe: MlxLogCb = { _, _ in }
        DispatchQueue.concurrentPerform(iterations: 1000) { index in
            switch index % 3 {
            case 0: ds_mlx_set_log_cb(probe)
            case 1: ds_mlx_set_log_cb(nil)
            default: mlxLogSink.emit(MlxLogLevel.warn, "concurrent probe")
            }
        }
    }

    func testFluidSinkSurvivesConcurrentRegistrationAndEmit() {
        let probe: FluidLogCb = { _, _ in }
        DispatchQueue.concurrentPerform(iterations: 1000) { index in
            switch index % 3 {
            case 0: ds_fluid_set_log_cb(probe)
            case 1: ds_fluid_set_log_cb(nil)
            default: fluidLogSink.emit(FluidLogLevel.warn, "concurrent probe")
            }
        }
    }
}
