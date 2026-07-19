import XCTest

@testable import dskokoro

final class TtsAbiTests: XCTestCase {
    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(dsk_synthesize_phonemes(nil, nil, 1.0, nil, nil), 4)
    }
}
