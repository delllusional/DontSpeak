import XCTest

@testable import smkokoro

/// Covers the four scenarios called out in the doc comment on `legacySegmentDidReset`
/// (shim.swift): the shared-prefix-ratio check and the phrase-gap check are meant to
/// agree on same-phrase revisions and disagree (in the heuristic's favor) on genuine
/// resets, with one documented blind spot (a longer reset that coincidentally shares
/// a prefix with the previous phrase, arriving inside the gap window).
final class LegacySegmentResetTests: XCTestCase {
    /// In-flight revision within the same phrase (e.g. digit re-grouping) must not be
    /// treated as a reset, regardless of the callback gap.
    func testSamePhraseGrowsIsNotAReset() {
        XCTAssertFalse(
            legacySegmentDidReset(previous: "Testing one", new: "Testing 12", gapSeconds: 0.1))
        XCTAssertFalse(
            legacySegmentDidReset(previous: "Testing one", new: "Testing 12", gapSeconds: nil))
    }

    /// A genuine reset that shrinks and shares almost no prefix with the previous text
    /// is caught by the ratio check alone, even with no measurable gap.
    func testGenuineShorterResetIsCaught() {
        XCTAssertTrue(
            legacySegmentDidReset(previous: "the quick brown fox", new: "hello", gapSeconds: nil))
    }

    /// A genuine reset that grows into a longer, unrelated phrase needs the phrase-gap
    /// signal to be caught — the ratio check alone can't fire on growth (only shrink OR
    /// gap trips the second half of the condition), so a >= 0.65s gap is required even
    /// though the shared-prefix ratio is already well under 0.5.
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

    /// System Speech repeats the last unchanged partial at a phrase boundary. The duplicate
    /// must not replace the timestamp of the last actual text change, or the genuine ~2s pause
    /// measured on hardware appears as the 0.3s gap from that duplicate to the replacement.
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

    /// Documented limitation: a longer reset that coincidentally shares >= 50% of its
    /// prefix with the previous phrase is NOT caught, even across the gap threshold —
    /// the ratio check suppresses it before the gap is consulted. This test pins the
    /// known blind spot rather than asserting a fix; if this ever starts passing, the
    /// heuristic changed and the doc comment above `legacySegmentDidReset` needs an update.
    func testGenuineLongerResetWithCoincidentalSharedPrefixIsMissed() {
        XCTAssertFalse(
            legacySegmentDidReset(
                previous: "the meeting", new: "the meeting room booking confirmed for noon",
                gapSeconds: 2.0))
    }

    /// Hardware telemetry included ordinary low-prefix in-flight revisions at 0.306s. Keeping
    /// the calibrated 0.65s guard prevents such a revision from committing a false phrase.
    func testObservedLowPrefixRevisionDoesNotTriggerBelowThreshold() {
        XCTAssertFalse(
            legacySegmentDidReset(
                previous: "A completely different", new: "I completely different",
                gapSeconds: 0.306))
    }

    /// Empty previous text has nothing to reset from.
    func testEmptyPreviousIsNeverAReset() {
        XCTAssertFalse(legacySegmentDidReset(previous: "", new: "anything", gapSeconds: 5.0))
    }
}
