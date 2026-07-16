import XCTest

@testable import smkokoro

/// Pins legacySegmentDidReset: prefix-ratio + gap agree on same-phrase / genuine resets;
/// one documented blind spot (long reset with coincidental >=50% prefix inside gap window).
final class LegacySegmentResetTests: XCTestCase {
    /// Same-phrase revision (digit re-grouping) is not a reset at any gap.
    func testSamePhraseGrowsIsNotAReset() {
        XCTAssertFalse(
            legacySegmentDidReset(previous: "Testing one", new: "Testing 12", gapSeconds: 0.1))
        XCTAssertFalse(
            legacySegmentDidReset(previous: "Testing one", new: "Testing 12", gapSeconds: nil))
    }

    /// Shrink + low prefix → ratio alone catches reset.
    func testGenuineShorterResetIsCaught() {
        XCTAssertTrue(
            legacySegmentDidReset(previous: "the quick brown fox", new: "hello", gapSeconds: nil))
    }

    /// Growth + low prefix needs gap ≥ 0.65s (ratio alone cannot fire on growth).
    func testGenuineLongerResetWithNoSharedPrefixNeedsThePhraseGap() {
        XCTAssertTrue(
            legacySegmentDidReset(
                previous: "hello", new: "a completely different sentence entirely",
                gapSeconds: 0.65))
        XCTAssertFalse(
            legacySegmentDidReset(
                previous: "hello", new: "a completely different sentence entirely",
                gapSeconds: 0.649))
    }

    /// Duplicate partial must not refresh change timestamp (hides real pause gap).
    func testDuplicatePartialDoesNotHideGenuineResetGap() {
        let first = legacyPartialTiming(
            previous: "", new: "Hello", lastChangedAt: nil, now: 0.0)
        XCTAssertNil(first.gapSeconds)
        XCTAssertEqual(first.lastChangedAt, 0.0)

        let duplicate = legacyPartialTiming(
            previous: "Hello", new: "Hello", lastChangedAt: first.lastChangedAt, now: 1.95)
        XCTAssertEqual(duplicate.gapSeconds!, 1.95, accuracy: 0.001)
        XCTAssertEqual(duplicate.lastChangedAt, 0.0)

        let replacement = legacyPartialTiming(
            previous: "Hello", new: "Completely", lastChangedAt: duplicate.lastChangedAt,
            now: 2.26)
        XCTAssertEqual(replacement.gapSeconds!, 2.26, accuracy: 0.001)
        XCTAssertEqual(replacement.lastChangedAt, 2.26)
        XCTAssertTrue(
            legacySegmentDidReset(
                previous: "Hello", new: "Completely",
                gapSeconds: replacement.gapSeconds))
    }

    /// Known blind spot: coincidental ≥50% prefix is not caught even with gap.
    /// If this starts passing, update legacySegmentDidReset doc.
    func testGenuineLongerResetWithCoincidentalSharedPrefixIsMissed() {
        XCTAssertFalse(
            legacySegmentDidReset(
                previous: "the meeting", new: "the meeting room booking confirmed for noon",
                gapSeconds: 2.0))
    }

    /// Hardware saw low-prefix in-flight revisions at 0.306s — 0.65s guard blocks false commit.
    func testObservedLowPrefixRevisionDoesNotTriggerBelowThreshold() {
        XCTAssertFalse(
            legacySegmentDidReset(
                previous: "A completely different", new: "I completely different",
                gapSeconds: 0.306))
    }

    func testEmptyPreviousIsNeverAReset() {
        XCTAssertFalse(legacySegmentDidReset(previous: "", new: "anything", gapSeconds: 5.0))
    }
}
