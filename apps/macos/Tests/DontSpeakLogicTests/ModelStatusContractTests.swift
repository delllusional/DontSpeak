import Foundation
import XCTest

@testable import DontSpeakLogic

/// Pins the Swift model_status mirror to the ds-status wire shape (R04F02).
/// Golden sample mirrors `ds-status::tests::sample()` field layout.
final class ModelStatusContractTests: XCTestCase {
    /// Canonical idle payload — same closed-set defaults as the Rust round-trip fixture.
    private static let sampleJSON = """
        {
          "seq": 0,
          "activity": {
            "caps": false,
            "caps_active": false,
            "recording": false,
            "speaking": false,
            "speaker": null,
            "utterance_id": null,
            "voice": null,
            "language": null,
            "warning": null,
            "muted": false
          },
          "tts": {
            "engine": "system",
            "model": null,
            "language": null,
            "provider": null,
            "status": {"state": "missing", "progress": 0.0, "error": null},
            "recent_utterances": []
          },
          "stt": {
            "engine": "built_in",
            "provider": null,
            "status": {"state": "missing", "progress": 0.0, "error": null},
            "voice_key": null
          },
          "diarization": {
            "status": {"state": "missing", "progress": 0.0, "error": null},
            "enabled": false,
            "provider": null,
            "speakers": [],
            "activity_threshold": 0.5
          },
          "dictation": {
            "state": "hidden",
            "text": "",
            "can_paste": true
          },
          "stats": {
            "tts": {
              "rtf_min": 0.0, "rtf_avg": 0.0, "rtf_max": 0.0,
              "ttfa_min_ms": 0.0, "ttfa_avg_ms": 0.0, "ttfa_max_ms": 0.0,
              "utterances": 0, "audio_secs": 0.0, "failures": 0, "queued": 0
            },
            "stt": {
              "rtf_min": 0.0, "rtf_avg": 0.0, "rtf_max": 0.0,
              "transcriptions": 0, "audio_secs": 0.0, "failures": 0
            },
            "lifetime": {"tts_secs": 0, "stt_secs": 0}
          },
          "tray": ["stt", "tts"],
          "downloads": [],
          "agents": false
        }
        """

    func testSamplePayloadDecodesEveryRootField() throws {
        let dto = try JSONDecoder().decode(
            ModelStatusDTO.self, from: Data(Self.sampleJSON.utf8))

        XCTAssertEqual(dto.seq, 0)
        XCTAssertFalse(dto.activity.caps)
        XCTAssertFalse(dto.activity.capsActive)
        XCTAssertFalse(dto.activity.recording)
        XCTAssertFalse(dto.activity.speaking)
        XCTAssertNil(dto.activity.speaker)
        XCTAssertNil(dto.activity.utteranceId)
        XCTAssertNil(dto.activity.voice)
        XCTAssertNil(dto.activity.language)
        XCTAssertNil(dto.activity.warning)
        XCTAssertFalse(dto.activity.muted)

        XCTAssertEqual(dto.tts.engine, "system")
        XCTAssertNil(dto.tts.model)
        XCTAssertNil(dto.tts.language)
        XCTAssertNil(dto.tts.provider)
        XCTAssertEqual(dto.tts.status?.state, "missing")
        XCTAssertTrue(dto.tts.recentUtterances.isEmpty)

        XCTAssertEqual(dto.stt.engine, "built_in")
        XCTAssertNil(dto.stt.provider)
        XCTAssertEqual(dto.stt.status?.state, "missing")
        XCTAssertNil(dto.stt.voiceKey)

        XCTAssertEqual(dto.diarization.status.state, "missing")
        XCTAssertFalse(dto.diarization.enabled)
        XCTAssertNil(dto.diarization.provider)
        XCTAssertEqual(dto.diarization.speakers, [])
        XCTAssertEqual(dto.diarization.activityThreshold, 0.5)

        XCTAssertEqual(dto.dictation.state, "hidden")
        XCTAssertEqual(dto.dictation.text, "")
        XCTAssertTrue(dto.dictation.canPaste)

        XCTAssertEqual(dto.stats.tts.queued, 0)
        XCTAssertEqual(dto.stats.stt.transcriptions, 0)
        XCTAssertEqual(dto.stats.lifetime.ttsSecs, 0)

        XCTAssertEqual(dto.tray, ["stt", "tts"])
        XCTAssertTrue(dto.downloads.isEmpty)
        XCTAssertFalse(dto.agents)
    }

    func testSampleJSONObjectHasExactlyTenRootKeys() throws {
        let root = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: Data(Self.sampleJSON.utf8)) as? [String: Any])
        let keys = Set(root.keys)
        XCTAssertEqual(keys.count, 10, "no duplicated root-level engine fields")
        for key in [
            "seq", "activity", "tts", "stt", "diarization",
            "dictation", "stats", "tray", "downloads", "agents",
        ] {
            XCTAssertTrue(keys.contains(key), "missing root field \(key)")
        }
    }

    func testUtteranceAndDownloadTelemetryDecode() throws {
        let dto = try JSONDecoder().decode(
            ModelStatusDTO.self,
            from: Data(
                """
                {
                  "seq": 7,
                  "activity": {
                    "caps": true, "caps_active": true, "recording": false,
                    "speaking": true, "speaker": "claude", "utterance_id": 12,
                    "voice": "if_sara", "language": "it", "warning": null, "muted": false
                  },
                  "tts": {
                    "engine": "built_in", "model": "kokoro", "language": "it",
                    "provider": "mlx",
                    "status": {"state": "running", "progress": 0, "error": null},
                    "recent_utterances": [{
                      "id": 11, "voice": "if_sara", "language": "it",
                      "warning": null, "outcome": "spoken"
                    }]
                  },
                  "stt": {
                    "engine": "built_in", "provider": "cpu",
                    "status": {"state": "idle", "progress": 0, "error": null},
                    "voice_key": null
                  },
                  "diarization": {
                    "status": {"state": "running", "progress": 1.0, "error": null},
                    "enabled": true, "provider": "mlx", "speakers": ["Alex"],
                    "activity_threshold": 0.72
                  },
                  "dictation": {"state": "hidden", "text": "", "can_paste": true},
                  "stats": {
                    "tts": {
                      "rtf_min": 1.0, "rtf_avg": 1.2, "rtf_max": 1.5,
                      "ttfa_min_ms": 0, "ttfa_avg_ms": 0, "ttfa_max_ms": 0,
                      "utterances": 7, "audio_secs": 33.5, "failures": 2, "queued": 4
                    },
                    "stt": {
                      "rtf_min": 0, "rtf_avg": 0.4, "rtf_max": 0,
                      "transcriptions": 3, "audio_secs": 9.0, "failures": 1
                    },
                    "lifetime": {"tts_secs": 100, "stt_secs": 50}
                  },
                  "tray": ["stt", "tts_animated"],
                  "downloads": [{
                    "target": "kokoro_model", "done_bytes": 25, "total_bytes": 100,
                    "start_bytes": 5, "elapsed_seconds": 2
                  }],
                  "agents": true
                }
                """.utf8))

        XCTAssertEqual(dto.seq, 7)
        XCTAssertEqual(dto.activity.speaker, "claude")
        XCTAssertEqual(dto.activity.utteranceId, 12)
        XCTAssertEqual(dto.activity.voice, "if_sara")
        XCTAssertEqual(dto.tts.model, .kokoro)
        XCTAssertEqual(dto.tts.recentUtterances.first?.id, 11)
        XCTAssertEqual(dto.tts.recentUtterances.first?.voice, "if_sara")
        XCTAssertEqual(dto.tts.recentUtterances.first?.outcome, "spoken")
        XCTAssertEqual(dto.downloads.count, 1)
        XCTAssertEqual(dto.downloads[0].doneBytes, 25)
        XCTAssertEqual(dto.downloads[0].startBytes, 5)
        XCTAssertEqual(dto.downloads[0].elapsedSeconds, 2)
        XCTAssertEqual(dto.diarization.speakers, ["Alex"])
        // Realized backend: a running row names its provider.
        XCTAssertEqual(dto.diarization.provider, "mlx")
        XCTAssertTrue(dto.agents)
        XCTAssertEqual(dto.stats.tts.queued, 4)
    }

    func testUnknownTtsModelFailsClosed() {
        let json = """
            {
              "seq": 0,
              "activity": {
                "caps": false, "caps_active": false, "recording": false,
                "speaking": false, "speaker": null, "utterance_id": null,
                "voice": null, "language": null, "warning": null, "muted": false
              },
              "tts": {
                "engine": "built_in", "model": "future_model", "language": "en",
                "provider": "cpu",
                "status": {"state": "running", "progress": 0, "error": null},
                "recent_utterances": []
              },
              "stt": {
                "engine": "built_in", "provider": null,
                "status": {"state": "missing", "progress": 0, "error": null},
                "voice_key": null
              },
              "diarization": {
                "status": {"state": "missing", "progress": 0, "error": null},
                "enabled": false, "provider": null, "speakers": [],
                "activity_threshold": 0.5
              },
              "dictation": {"state": "hidden", "text": "", "can_paste": true},
              "stats": {
                "tts": {
                  "rtf_min": 0, "rtf_avg": 0, "rtf_max": 0,
                  "ttfa_min_ms": 0, "ttfa_avg_ms": 0, "ttfa_max_ms": 0,
                  "utterances": 0, "audio_secs": 0, "failures": 0, "queued": 0
                },
                "stt": {
                  "rtf_min": 0, "rtf_avg": 0, "rtf_max": 0,
                  "transcriptions": 0, "audio_secs": 0, "failures": 0
                },
                "lifetime": {"tts_secs": 0, "stt_secs": 0}
              },
              "tray": ["stt", "tts"],
              "downloads": [],
              "agents": false
            }
            """
        XCTAssertThrowsError(
            try JSONDecoder().decode(ModelStatusDTO.self, from: Data(json.utf8)))
    }
}
