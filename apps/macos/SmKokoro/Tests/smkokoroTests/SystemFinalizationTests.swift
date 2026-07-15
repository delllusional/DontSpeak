import XCTest

@testable import smkokoro

final class SystemFinalizationTests: XCTestCase {
    /// Regression from the macOS 15 live repro: the final callback dropped a real suffix that
    /// the last partial (and therefore the popup) had already shown.
    func testStrictPrefixFinalKeepsCompletePartial() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "Recognition produces the correct text Submitted",
                finalSegment: "Recognition produces the correct text"),
            "Recognition produces the correct text Submitted")
    }

    func testEmptyFinalKeepsNonemptyPartial() {
        XCTAssertEqual(
            systemSettledSegment(latestPartial: "Keep the whole sentence", finalSegment: ""),
            "Keep the whole sentence")
    }

    func testNoSpeechTerminalKeepsLastPopupPartial() {
        let run = LegacyRun()
        run.recordPartial("Hello from the popup")
        run.recordPartial("")
        run.finishNoSpeech()
        XCTAssertEqual(run.text, "Hello from the popup")
    }

    func testWordPrefixComparisonIgnoresFinalFormatting() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "verify the entire sentence and fix it",
                finalSegment: "Verify the entire sentence."),
            "verify the entire sentence and fix it")
    }

    func testSameLengthFinalCorrectionWins() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "open the get repository",
                finalSegment: "Open the Git repository."),
            "Open the Git repository.")
    }

    func testShorterNonPrefixFinalCorrectionWins() {
        XCTAssertEqual(
            systemSettledSegment(
                latestPartial: "incorrect speculative ending",
                finalSegment: "Correct ending"),
            "Correct ending")
    }
}
