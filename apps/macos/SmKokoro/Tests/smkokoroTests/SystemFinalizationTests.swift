import XCTest

@testable import smkokoro

final class SystemFinalizationTests: XCTestCase {
    /// Regression from the macOS 15 live repro: the final callback dropped a real suffix that
    /// the last partial (and therefore the popup) had already shown.
    func testMacOS15StrictPrefixWorkaroundKeepsCompletePartial() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "Recognition produces the correct text Submitted",
                incomingSegment: "Recognition produces the correct text",
                preserveStrictPrefix: true),
            "Recognition produces the correct text Submitted")
    }

    func testStrictPrefixFinalCorrectionWinsWithoutWorkaround() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "Open the report tomorrow",
                incomingSegment: "Open the report",
                preserveStrictPrefix: false),
            "Open the report")
    }

    func testEmptyFinalKeepsNonemptyPartial() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "Keep the whole sentence",
                incomingSegment: "",
                preserveStrictPrefix: false),
            "Keep the whole sentence")
    }

    func testNoSpeechTerminalKeepsLastPopupPartial() {
        let run = LegacyRun()
        run.recordPartial("Hello from the popup")
        run.recordPartial("")
        run.finishNoSpeech()
        XCTAssertEqual(run.text, "Hello from the popup")
    }

    /// #84: a transient empty callback followed by the same phrase must not commit the phrase
    /// and then expose its resumed copy as a second segment.
    func testTransientEmptyPartialThenSameTextDoesNotDuplicate() {
        let run = LegacyRun()
        run.recordPartial("hello world")
        run.recordPartial("")
        run.recordPartial("hello world")
        XCTAssertEqual(run.hypothesis(), "hello world")
        XCTAssertEqual(
            run.finalJoined("hello world", preserveStrictPrefix: false),
            "hello world")
    }

    /// Ignoring an empty callback must leave the prior segment available for the next genuine
    /// low-prefix reset instead of dropping either side of the phrase boundary.
    func testEmptyPartialThenNewSegmentCommitsPreviousOnce() {
        let run = LegacyRun()
        run.recordPartial("the first phrase is complete")
        run.recordPartial("")
        run.recordPartial("next phrase")
        XCTAssertEqual(run.hypothesis(), "the first phrase is complete next phrase")
        XCTAssertEqual(
            run.finalJoined("next phrase", preserveStrictPrefix: false),
            "the first phrase is complete next phrase")
    }

    func testWordPrefixComparisonIgnoresFinalFormatting() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "verify the entire sentence and fix it",
                incomingSegment: "Verify the entire sentence.",
                preserveStrictPrefix: true),
            "verify the entire sentence and fix it")
    }

    func testSameLengthFinalCorrectionWins() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "open the get repository",
                incomingSegment: "Open the Git repository.",
                preserveStrictPrefix: true),
            "Open the Git repository.")
    }

    func testShorterNonPrefixFinalCorrectionWins() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "incorrect speculative ending",
                incomingSegment: "Correct ending",
                preserveStrictPrefix: true),
            "Correct ending")
    }

    func testStrictPrefixPartialCorrectionReplacesSpeculation() {
        let run = LegacyRun()
        run.recordPartial("Open the report tomorrow")
        run.recordPartial("Open the report")
        XCTAssertEqual(run.hypothesis(), "Open the report")
    }
}
