import XCTest

@testable import DontSpeakMLX

final class TtsAbiTests: XCTestCase {
    /// Missing cb must fail before model work (success promises exactly one callback).
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(ds_mlx_tts_synthesize2(nil, nil, nil, 1.0, nil, nil, nil), 4)
    }

    /// Hand-maintained Rust-registry mirror pin.
    func testTtsParamMirrorPinsDeclaredKeysPerModel() {
        XCTAssertEqual(Set(ttsParamMirror.keys), ["kokoro", "chatterbox", "qwen", "omnivoice"])
        XCTAssertEqual(ttsParamMirror["kokoro"], [:])
        XCTAssertEqual(ttsParamMirror["chatterbox"], ["exaggeration": true])
        XCTAssertEqual(ttsParamMirror["qwen"], ["repetition_penalty": true])
        XCTAssertEqual(ttsParamMirror["omnivoice"], ["steps": true, "seed": true])
    }

    func testParamsDecodeClassifiesDeclaredUnknownAndMalformed() {
        XCTAssertEqual(
            ttsParamsDecode(model: "chatterbox", json: #"{"exaggeration":0.7,"bogus":1}"#),
            TtsParamDecode(
                applied: ["exaggeration"],
                unknown: ["bogus"],
                values: TtsAppliedParams(chatterboxExaggeration: 0.7)))
        XCTAssertEqual(
            ttsParamsDecode(model: "omnivoice", json: #"{"steps":16,"seed":-1}"#),
            TtsParamDecode(
                applied: ["seed", "steps"],
                values: TtsAppliedParams(omniVoiceSteps: 16, omniVoiceSeed: -1)))
        XCTAssertEqual(
            ttsParamsDecode(model: "qwen", json: #"{"repetition_penalty":1.25}"#),
            TtsParamDecode(
                applied: ["repetition_penalty"],
                values: TtsAppliedParams(qwenRepetitionPenalty: 1.25)))
        XCTAssertEqual(ttsParamsDecode(model: "kokoro", json: "{}"), TtsParamDecode())
        XCTAssertNil(ttsParamsDecode(model: "qwen", json: "not json"))
        XCTAssertNil(ttsParamsDecode(model: "omnivoice", json: #"{"steps":16.5,"seed":-1}"#))
        XCTAssertNil(ttsParamsDecode(model: "omnivoice", json: #"{"steps":65,"seed":-1}"#))
        XCTAssertNil(ttsParamsDecode(model: "chatterbox", json: #"{"exaggeration":true}"#))
    }

    func testStableOmniVoiceSeedMatchesOrtBackend() {
        XCTAssertEqual(
            stableOmniVoiceSeed(language: "en", instruct: ""),
            0xc2ef_df18_f053_12de)
        XCTAssertNotEqual(
            stableOmniVoiceSeed(language: "en", instruct: "female"),
            stableOmniVoiceSeed(language: "en", instruct: "male"))
    }

    func testInitRejectsMissingAndUnknownModels() {
        XCTAssertEqual(ds_mlx_tts_init(nil, nil), 3)
        "unknown".withCString { model in
            "/tmp".withCString { directory in
                XCTAssertEqual(ds_mlx_tts_init(model, directory), 3)
            }
        }
    }

    func testAllBorrowedResultFunctionsRejectNullCallbacks() {
        XCTAssertEqual(ds_mlx_transcribe(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_mlx_asr_stream_push(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_mlx_asr_stream_finish(nil, nil), 4)
        XCTAssertEqual(ds_mlx_diarize(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_mlx_diar_embed(nil, 0, 16_000, nil, nil), 4)
    }

    func testBufferedPartialCadenceStartsAtOneSecondAndAdvancesByOneSecond() {
        XCTAssertFalse(asrShouldDecodePartial(totalSamples: 15_999, lastDecodedSamples: 0))
        XCTAssertTrue(asrShouldDecodePartial(totalSamples: 16_000, lastDecodedSamples: 0))
        XCTAssertFalse(asrShouldDecodePartial(totalSamples: 31_999, lastDecodedSamples: 16_000))
        XCTAssertTrue(asrShouldDecodePartial(totalSamples: 32_000, lastDecodedSamples: 16_000))
    }

    func testStreamShutdownIsIdempotentWithoutUnloadingTheGlobalApi() {
        ds_mlx_asr_stream_shutdown()
        ds_mlx_asr_stream_shutdown()
        XCTAssertEqual(ds_mlx_transcribe(nil, 0, 16_000, nil, nil), 4)
    }
}
