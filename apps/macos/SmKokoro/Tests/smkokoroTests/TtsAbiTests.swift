import XCTest

@testable import smkokoro

final class TtsAbiTests: XCTestCase {
    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(smk_synthesize_phonemes(nil, nil, 1.0, nil, nil), 4)
    }
}
