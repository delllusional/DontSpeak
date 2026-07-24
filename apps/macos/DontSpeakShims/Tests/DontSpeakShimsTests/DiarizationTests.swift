import XCTest

@testable import DontSpeakMLX

final class DiarizationTests: XCTestCase {
    func testSpeakerEmbeddingRangesExcludeCrossTalk() {
        let ranges = [
            LabeledSampleRange(speaker: "0", start: 0, end: 100),
            LabeledSampleRange(speaker: "1", start: 25, end: 50),
            LabeledSampleRange(speaker: "1", start: 80, end: 120),
        ]
        XCTAssertEqual(exclusiveRanges(for: "0", in: ranges), [0..<25, 50..<80])
        XCTAssertEqual(exclusiveRanges(for: "1", in: ranges), [100..<120])
    }
}
