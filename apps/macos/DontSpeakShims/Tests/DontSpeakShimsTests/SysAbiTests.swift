import XCTest

@testable import DontSpeakSys

/// System-STT ABI edges only -- no model files, no network, no TCC prompt, no recognizer, no
/// OS-service query, no unbounded `sysRunBlocking`. Every call below returns before a Speech
/// recognizer or analyzer is constructed. Extending this suite to an entry point that reaches
/// `sysRunBlocking` or a live Speech object is a bug, not coverage.
///
/// Deliberately never called here, each for a reason a reader would otherwise re-litigate:
///   * `ds_sys_available` -- "non-prompting" is not "non-blocking". On macOS 26 it awaits
///     `SpeechTranscriber.supportedLocales` / `.installedLocales`, a live OS asset-inventory
///     query, through a `sysRunBlocking` that has NO timeout; on 14-25 it constructs a real
///     `SFSpeechRecognizer`. A wedged speech daemon would park the XCTest thread forever, and
///     CI declares no `timeout-minutes` (#209), so that is a held runner rather than a red
///     test. Its export is asserted at build time by `verify_shim_exports` instead.
///   * `ds_sys_authorize` -- macOS 26 reaches `AssetInventory` `downloadAndInstall()` (a real
///     network download; CI pins macos-26); 14-25 raises the Speech-Recognition TCC prompt
///     with a 120 s blocking poll.
///   * `ds_sys_stream_start` -- same model download plus `SpeechAnalyzer.start`, and it leaves
///     a live session in the process-global `sysStream` that every later test would inherit.
///   * `ds_sys_transcribe` with `n > 0` -- both live paths.
final class SysAbiTests: XCTestCase {
    /// Empty input short-circuits to "" before any recognition work.
    func testTranscribeWithNoSamplesReportsEmptyText() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_transcribe(nil, 0, 16_000, nil, cb), 0)
    }

    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testAllBorrowedResultFunctionsRejectNullCallbacks() {
        XCTAssertEqual(ds_sys_transcribe(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_sys_stream_push(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_sys_stream_finish(nil, nil), 4)
    }

    /// An empty chunk still reaches the session check without constructing a live Speech object.
    func testZeroLengthStreamPushBeforeStartReportsNotStarted() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_stream_push(nil, 0, 16_000, nil, cb), 2)
    }

    /// No session was started, so finish reports the empty transcript rather than failing.
    func testStreamFinishWithoutASessionReportsEmptyText() {
        let cb: SysStrCb = { _, _ in }
        XCTAssertEqual(ds_sys_stream_finish(nil, cb), 0)
    }

    /// Registering and clearing the sink is a pure state write on both edges.
    func testSetLogCallbackAcceptsBothEdges() {
        let cb: SysLogCb = { _, _ in }
        ds_sys_set_log_cb(cb)
        ds_sys_set_log_cb(nil)
    }
}
