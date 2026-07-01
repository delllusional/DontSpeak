import Foundation
import XCTest

@testable import DontSpeakLogic

/// The `stats` wire decode + the `EngineStats.from(_:)` defaulting rules — the same
/// seam the Windows app covers in winui.tests/HealthSnapshotTests, so the two UIs
/// can't drift on how a partial payload reads.
final class EngineStatsTests: XCTestCase {
    private func decode(_ json: String) throws -> StatsDTO {
        try JSONDecoder().decode(StatsDTO.self, from: Data(json.utf8))
    }

    /// A nil DTO (stats absent from the payload) is every struct default — including
    /// diarization's 0.7 clustering threshold.
    func testNilDtoIsAllDefaults() {
        let s = EngineStats.from(nil)
        XCTAssertEqual(s, EngineStats())
        XCTAssertEqual(s.diarization.clusteringThreshold, 0.7)
        XCTAssertFalse(s.diarization.enabled)
    }

    /// An empty stats object decodes (all blocks nil) and maps to the same defaults.
    func testEmptyStatsObjectIsAllDefaults() throws {
        XCTAssertEqual(EngineStats.from(try decode("{}")), EngineStats())
    }

    /// The full happy path: every block maps snake_case wire keys onto its group.
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

    /// A present block with missing leaves falls to the PER-FIELD defaults (numbers → 0,
    /// flags → false, speakers → []) — not the struct defaults. The documented quirk:
    /// `clusteringThreshold` lands on 0 (not 0.7) once a `diarization` block is present
    /// but omits the key, matching the old `[String: Any]` walk exactly.
    func testPresentBlockWithMissingLeavesFallsToZero() throws {
        let s = EngineStats.from(try decode(#"{"diarization": {"enabled": true}, "tts": {}}"#))
        XCTAssertEqual(s.diarization.clusteringThreshold, 0)  // NOT 0.7 — block present
        XCTAssertEqual(s.diarization.speakers, [])
        XCTAssertEqual(s.tts.utterances, 0)
        XCTAssertEqual(s.tts.rtfAvg, 0)
        // Blocks that stayed absent keep their struct defaults.
        XCTAssertEqual(s.stt, EngineStats.Stt())
        XCTAssertEqual(s.lifetime, EngineStats.Lifetime())
    }

    /// Unknown wire keys (a newer engine) never break the decode — `present`,
    /// `speaker_threshold` and `loaded` are real keys the engine sends that this
    /// app deliberately doesn't read.
    func testUnknownKeysAreIgnored() throws {
        let dto = try decode(
            """
            {"diarization": {"enabled": true, "present": true, "speaker_threshold": 0.5},
             "loaded": {"tts": true, "stt": false},
             "some_future_block": {"x": 1}}
            """)
        XCTAssertTrue(EngineStats.from(dto).diarization.enabled)
    }

    /// Lifetime totals arrive as JSON integers (engine u64) and must decode into Double.
    func testLifetimeIntegerSecondsDecode() throws {
        let s = EngineStats.from(try decode(#"{"lifetime": {"tts_secs": 12345, "stt_secs": 0}}"#))
        XCTAssertEqual(s.lifetime.ttsSecs, 12345)
    }
}
