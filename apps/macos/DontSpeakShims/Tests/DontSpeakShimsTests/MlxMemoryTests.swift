import XCTest

@testable import DontSpeakMLX

final class MlxMemoryTests: XCTestCase {
    func testCacheLimitIsBounded() {
        XCTAssertEqual(mlxCacheLimitBytes, 256 * 1024 * 1024)
    }

    func testMemoryLogLineIsMachineReadable() {
        let stats = MlxMemoryStats(
            activeBytes: 1_024,
            cacheBytes: 2_048,
            peakBytes: 4_096,
            cacheLimitBytes: 8_192)

        XCTAssertEqual(
            stats.logLine(phase: "tts_synthesize"),
            "memory phase=tts_synthesize active_bytes=1024 cache_bytes=2048 "
                + "peak_bytes=4096 cache_limit_bytes=8192")
    }
}
