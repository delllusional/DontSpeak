import XCTest

@testable import DontSpeakLogic

final class StatusYieldTests: XCTestCase {
    /// The very first sample is always delivered, regardless of seq/running.
    func testFirstSampleAlwaysYields() {
        XCTAssertTrue(
            statusShouldYield(delivered: false, seq: 0, since: 0, running: true, lastRunning: true)
        )
    }

    /// Idle: the ~1 s timeout returns the SAME seq and the same running state → no yield
    /// (this is the dedup that stops the ~1 Hz idle churn).
    func testIdleUnchangedDoesNotYield() {
        XCTAssertFalse(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: true, lastRunning: true)
        )
    }

    /// A real engine-side status change advances the gate sequence → yield.
    func testSeqAdvanceYields() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 8, since: 7, running: true, lastRunning: true)
        )
    }

    /// REGRESSION (#12): the engine goes DOWN. `engineRunning` is an external probe not
    /// carried in the gate seq, so when the engine stops the seq FREEZES (seq == since).
    /// Gating on seq alone would never yield the down state — the menu-bar dot would stay a
    /// stale "running". Reacting to the running flip must still yield.
    func testEngineDownYieldsEvenWithFrozenSeq() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: false, lastRunning: true),
            "engine-down must yield even though the gate seq is frozen"
        )
    }

    /// Engine comes back UP while the seq is still frozen at the last down value → yield.
    func testEngineUpYieldsOnRunningFlip() {
        XCTAssertTrue(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: true, lastRunning: false)
        )
    }

    /// Down and STAYING down (running unchanged, seq frozen) → no repeated yields; the
    /// producer paces itself instead of churning.
    func testStaysDownDoesNotRepeatedlyYield() {
        XCTAssertFalse(
            statusShouldYield(delivered: true, seq: 7, since: 7, running: false, lastRunning: false)
        )
    }
}
