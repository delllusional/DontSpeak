import XCTest

@testable import DontSpeakMLX

final class TtsAbiTests: XCTestCase {
    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(ds_mlx_tts_synthesize(nil, nil, nil, 1.0, nil, nil, nil), 4)
    }

    /// Swift half of the cross-language params drift guard: the hand-maintained mirror
    /// must list exactly the Rust registry's declared keys per model (ds-tts's
    /// `mlx_params` test pins the Rust half). A Rust-side param addition that skips the
    /// mirror fails here.
    func testTtsParamMirrorPinsDeclaredKeysPerModel() {
        XCTAssertEqual(Set(ttsParamMirror.keys), ["kokoro", "chatterbox", "qwen", "omnivoice"])
        XCTAssertEqual(ttsParamMirror["kokoro"], [:])
        XCTAssertEqual(ttsParamMirror["chatterbox"], ["exaggeration": false])
        XCTAssertEqual(ttsParamMirror["qwen"], ["repetition_penalty": false])
        XCTAssertEqual(ttsParamMirror["omnivoice"], ["steps": false])
    }

    /// Pins the shim's decode behavior for params_json payloads: declared keys classify
    /// per the mirror, undeclared keys surface as unknown, malformed JSON is nil (the
    /// entry point logs and synthesizes anyway — params never fail the utterance).
    func testParamsDecodeClassifiesDeclaredUnknownAndMalformed() {
        XCTAssertEqual(
            ttsParamsDecode(model: "chatterbox", json: #"{"exaggeration":0.7,"bogus":1}"#),
            TtsParamDecode(applied: [], ignored: ["exaggeration"], unknown: ["bogus"]))
        XCTAssertEqual(
            ttsParamsDecode(model: "omnivoice", json: #"{"steps":16}"#),
            TtsParamDecode(applied: [], ignored: ["steps"], unknown: []))
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
