import XCTest

@testable import DontSpeakSys

/// System-STT ABI edges only — no model files, network, TCC, recognizer, OS-service
/// query, or unbounded `sysRunBlocking`. Every call returns before a Speech object is
/// constructed. Reaching `sysRunBlocking` or a live Speech object here is a bug.
///
/// Deliberately never called:
///   * `ds_sys_available` — "non-prompting" is not non-blocking. 26+ awaits locale
///     inventory via `sysRunBlocking` with no timeout; 14-25 constructs a real
///     SFSpeechRecognizer. A wedged speech daemon parks XCTest forever, and CI has
///     no `timeout-minutes` (#209). Export is asserted by `verify_shim_exports`.
///   * `ds_sys_authorize` — 26+ may `downloadAndInstall()`; 14-25 raises TCC with
///     a 120s blocking poll.
///   * `ds_sys_stream_start` — model download + SpeechAnalyzer.start; leaves a live
///     process-global session later tests would inherit.
///   * `ds_sys_transcribe` with `n > 0` — both live paths.
final class SysAbiTests: XCTestCase {
    /// Empty input short-circuits to "" before recognition work.
    func testTranscribeWithNoSamplesReportsEmptyText() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_transcribe(nil, 0, 16_000, nil, cb), 0)
    }

    /// Missing cb must fail before model work (success promises exactly one callback).
    func testAllBorrowedResultFunctionsRejectNullCallbacks() {
        XCTAssertEqual(ds_sys_transcribe(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_sys_stream_push(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_sys_stream_finish(nil, nil), 4)
    }

    /// Zero-length chunk still hits session check without a live Speech object.
    func testZeroLengthStreamPushBeforeStartReportsNotStarted() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_stream_push(nil, 0, 16_000, nil, cb), 2)
    }

    /// No session → empty transcript (rc 0).
    func testStreamFinishWithoutASessionReportsEmptyText() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_stream_finish(nil, cb), 0)
    }

    /// Registering and clearing the sink is a pure state write.
    func testSetLogCallbackAcceptsBothEdges() {
        let cb: SysLogCb = { _, _ in }
        ds_sys_set_log_cb(cb)
        ds_sys_set_log_cb(nil)
    }
}
