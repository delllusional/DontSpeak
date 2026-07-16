import XCTest

@testable import DontSpeakLogic

final class StatusYieldTests: XCTestCase {
    func testFirstSampleAlwaysYields() {
        XCTAssertTrue(
            statusShouldYield(delivered: false, seq: 0, since: 0, running: true, lastRunning: true)
        )
    }

    /// Idle timeout keeps seq unchanged — dedup stops ~1 Hz churn.
    func testIdleUnchangedDoesNotYield() {
        XCTAssertFalse(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: true, lastRunning: true)
        )
    }

    func testSeqAdvanceYields() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 8, since: 7, running: true, lastRunning: true)
        )
    }

    /// REGRESSION (#12): engineRunning is external to gate seq; stop freezes seq — must still yield.
    func testEngineDownYieldsEvenWithFrozenSeq() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: false, lastRunning: true),
            "engine-down must yield even though the gate seq is frozen"
        )
    }

    func testEngineUpYieldsOnRunningFlip() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: true, lastRunning: false)
        )
    }

    /// Staying down with frozen seq must not re-yield (producer paces).
    func testStaysDownDoesNotRepeatedlyYield() {
        XCTAssertFalse(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: false, lastRunning: false)
        )
    }
}
