import XCTest

@testable import DontSpeakFluid
@testable import DontSpeakMLX
@testable import DontSpeakSys

// `@convention(c)` closures capture nothing, so the probes count through file-scope globals.
// The takeover cases are single-threaded; the concurrency cases below register no-op probes
// precisely so they never touch these.
private nonisolated(unsafe) var sysHits = 0
private nonisolated(unsafe) var mlxHits = 0
private nonisolated(unsafe) var fluidHits = 0

private let sysProbe: SysLogCb = { _, _ in sysHits += 1 }
private let mlxProbe: MlxLogCb = { _, _ in mlxHits += 1 }
private let fluidProbe: FluidLogCb = { _, _ in fluidHits += 1 }

/// The per-family log sinks: a registered callback takes every line, and clearing it restores
/// the stderr fallback. Observed through each sink's fallback counter rather than by
/// redirecting the shared XCTest process's fd 2, which cannot deadlock on a full pipe.
///
/// One file can import all three modules only because every module-visible name in the three
/// targets is disjoint (`SysLogSink` / `MlxLogSink` / `FluidLogSink`, ...). If that ever
/// breaks, split this into three files -- one module each -- rather than reintroducing shared
/// names or papering over it with module qualification.
final class LogSinkTests: XCTestCase {
    /// The sinks are PROCESS-GLOBAL: a leaked one would change what SysAbiTests and
    /// FluidAbiTests do when they log.
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

    // Reentrancy: `emit` copies the callback pointer under the lock and releases the lock
    // before invoking it, so a sink can legitimately fire just after being deregistered. That
    // is benign (the Rust thunk is 'static in the Rust image) but it must never deadlock or
    // crash. The probes here are no-ops on purpose -- concurrent `@convention(c)` invocations
    // touching an unsynchronized counter would be a data race.

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
