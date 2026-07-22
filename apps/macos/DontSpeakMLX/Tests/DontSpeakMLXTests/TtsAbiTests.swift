import XCTest

@testable import DontSpeakMLX

final class TtsAbiTests: XCTestCase {
    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(ds_mlx_tts_synthesize2(nil, nil, nil, 1.0, nil, nil, nil), 4)
    }

    /// Literal pin for the hand-maintained Rust-registry mirror.
    func testTtsParamMirrorPinsDeclaredKeysPerModel() {
        XCTAssertEqual(Set(ttsParamMirror.keys), ["kokoro", "chatterbox", "qwen", "omnivoice"])
        XCTAssertEqual(ttsParamMirror["kokoro"], [:])
        XCTAssertEqual(ttsParamMirror["chatterbox"], ["exaggeration": false])
        XCTAssertEqual(ttsParamMirror["qwen"], ["repetition_penalty": false])
        XCTAssertEqual(ttsParamMirror["omnivoice"], ["steps": false, "seed": false])
    }

    func testParamsDecodeClassifiesDeclaredUnknownAndMalformed() {
        XCTAssertEqual(
            ttsParamsDecode(model: "chatterbox", json: #"{"exaggeration":0.7,"bogus":1}"#),
            TtsParamDecode(applied: [], ignored: ["exaggeration"], unknown: ["bogus"]))
        XCTAssertEqual(
            ttsParamsDecode(model: "omnivoice", json: #"{"steps":16,"seed":-1}"#),
            TtsParamDecode(applied: [], ignored: ["seed", "steps"], unknown: []))
        XCTAssertEqual(ttsParamsDecode(model: "kokoro", json: "{}"), TtsParamDecode())
        XCTAssertNil(ttsParamsDecode(model: "qwen", json: "not json"))
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
        XCTAssertEqual(ds_mlx_sys_transcribe(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_mlx_sys_stream_push(nil, 0, 16_000, nil, nil), 4)
        XCTAssertEqual(ds_mlx_sys_stream_finish(nil, nil), 4)
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
