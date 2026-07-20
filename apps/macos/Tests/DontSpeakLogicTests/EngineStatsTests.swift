import Foundation
import XCTest

@testable import DontSpeakLogic

/// Canonical metrics and diarization DTO decoding.
final class EngineStatsTests: XCTestCase {
    func testFullPayloadMapsEveryBlock() throws {
        let stats = try JSONDecoder().decode(
            StatsDTO.self,
            from: Data(
                """
                {"tts":{"rtf_min":1.0,"rtf_avg":1.2,"rtf_max":1.5,
                         "ttfa_min_ms":200,"ttfa_avg_ms":300,"ttfa_max_ms":500,
                         "utterances":7,"audio_secs":33.5,"failures":2},
                 "stt":{"rtf_min":0.3,"rtf_avg":0.4,"rtf_max":0.6,
                         "transcriptions":3,"audio_secs":9.0,"failures":1},
                 "lifetime":{"tts_secs":100,"stt_secs":50}}
                """.utf8))
        let diarization = try JSONDecoder().decode(
            DiarizationStatusDTO.self,
            from: Data(
                """
                {"status":{"state":"idle","progress":0,"error":null},
                 "enabled":true,"provider":"mlx","speakers":["alex"],
                 "activity_threshold":0.6,"future_detail":true}
                """.utf8))

        let s = EngineStats.from(stats)
        XCTAssertEqual(s.tts.rtfAvg, 1.2)
        XCTAssertEqual(s.tts.firstMaxMs, 500)
        XCTAssertEqual(s.tts.utterances, 7)
        XCTAssertEqual(s.tts.failures, 2)
        XCTAssertEqual(s.stt.transcriptions, 3)
        XCTAssertEqual(s.stt.failures, 1)
        XCTAssertTrue(diarization.enabled)
        XCTAssertEqual(diarization.speakers, ["alex"])
        XCTAssertEqual(diarization.activityThreshold, 0.6)
        XCTAssertEqual(diarization.provider, "mlx")
        XCTAssertEqual(s.lifetime.ttsSecs, 100)
        XCTAssertEqual(s.lifetime.sttSecs, 50)
    }
}
