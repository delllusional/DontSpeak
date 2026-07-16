import Foundation
import XCTest

@testable import DontSpeakLogic

/// stats wire decode + `EngineStats.from` defaults — same seam as Windows HealthSnapshotTests.
final class EngineStatsTests: XCTestCase {
    private func decode(_ json: String) throws -> StatsDTO {
        try JSONDecoder().decode(StatsDTO.self, from: Data(json.utf8))
    }

    func testNilDtoIsAllDefaults() {
        let s = EngineStats.from(nil)
        XCTAssertEqual(s, EngineStats())
        XCTAssertEqual(s.diarization.clusteringThreshold, 0.7)
        XCTAssertFalse(s.diarization.enabled)
    }

    func testEmptyStatsObjectIsAllDefaults() throws {
        XCTAssertEqual(EngineStats.from(try decode("{}")), EngineStats())
    }

    func testFullPayloadMapsEveryBlock() throws {
        let dto = try decode(
            """
            {"tts": {"rtf_avg": 1.2, "rtf_min": 1.0, "rtf_max": 1.5,
                     "first_avg_ms": 300, "first_min_ms": 200, "first_max_ms": 500,
                     "utterances": 7, "audio_secs": 33.5, "failures": 2},
             "stt": {"rtf_avg": 0.4, "rtf_min": 0.3, "rtf_max": 0.6,
                     "transcriptions": 3, "audio_secs": 9.0, "failures": 1},
             "diarization": {"enabled": true, "runtime": "coreml_ane",
                             "speakers": ["alex"], "clustering_threshold": 0.6},
             "lifetime": {"tts_secs": 100.5, "stt_secs": 50.25}}
            """)
        let s = EngineStats.from(dto)
        XCTAssertEqual(s.tts.rtfAvg, 1.2)
        XCTAssertEqual(s.tts.firstMaxMs, 500)
        XCTAssertEqual(s.tts.utterances, 7)
        XCTAssertEqual(s.tts.failures, 2)
        XCTAssertEqual(s.stt.transcriptions, 3)
        XCTAssertEqual(s.stt.failures, 1)
        XCTAssertTrue(s.diarization.enabled)
        XCTAssertEqual(s.diarization.speakers, ["alex"])
        XCTAssertEqual(s.diarization.clusteringThreshold, 0.6)
        XCTAssertEqual(s.diarization.runtime, "coreml_ane")
        XCTAssertEqual(s.lifetime.ttsSecs, 100.5)
        XCTAssertEqual(s.lifetime.sttSecs, 50.25)
    }

    /// Present block + missing leaf → per-field 0 (not struct defaults).
    /// Quirk: clusteringThreshold is 0 not 0.7 once diarization block is present.
    func testPresentBlockWithMissingLeavesFallsToZero() throws {
        let s = EngineStats.from(try decode(#"{"diarization": {"enabled": true}, "tts": {}}"#))
        XCTAssertEqual(s.diarization.clusteringThreshold, 0)  // NOT 0.7 — block present
        XCTAssertEqual(s.diarization.speakers, [])
        XCTAssertEqual(s.tts.utterances, 0)
        XCTAssertEqual(s.tts.rtfAvg, 0)
        // Absent blocks keep struct defaults.
        XCTAssertEqual(s.stt, EngineStats.Stt())
        XCTAssertEqual(s.lifetime, EngineStats.Lifetime())
    }

    /// Unknown wire keys (newer engine) must not break decode.
    func testUnknownKeysAreIgnored() throws {
        let dto = try decode(
            """
            {"diarization": {"enabled": true, "present": true, "speaker_threshold": 0.5},
             "loaded": {"tts": true, "stt": false},
             "some_future_block": {"x": 1}}
            """)
        XCTAssertTrue(EngineStats.from(dto).diarization.enabled)
    }

    /// Lifetime u64 JSON integers must decode into Double.
    func testLifetimeIntegerSecondsDecode() throws {
        let s = EngineStats.from(try decode(#"{"lifetime": {"tts_secs": 12345, "stt_secs": 0}}"#))
        XCTAssertEqual(s.lifetime.ttsSecs, 12345)
    }
}
