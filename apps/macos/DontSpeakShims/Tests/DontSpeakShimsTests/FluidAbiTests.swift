import XCTest

@testable import DontSpeakFluid

/// FluidAudio TTS ABI edges only -- no model files, no ANE, no network. `ds_fluid_tts_init`
/// is never called here (it would touch the ANE), so the manager stays nil for the whole
/// process and the not-initialized path is exercised deterministically.
final class FluidAbiTests: XCTestCase {
    /// Success promises exactly one callback, so a missing callback must fail before model work.
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, nil), 4)
    }

    /// Synthesizing before init returns the not-initialized rc (2), never a crash. The manager
    /// check precedes the phoneme check, so a nil phoneme pointer still reaches rc 2.
    func testSynthesisBeforeInitReportsNotInitialized() {
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }

    /// Shutdown with no live manager is a safe no-op (idempotent), and a following synthesize
    /// still reports not-initialized rather than touching a freed manager.
    func testShutdownWithoutInitIsSafe() {
        ds_fluid_tts_shutdown()
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }

    // ASR ABI edges. `ds_fluid_asr_init` / `ds_fluid_asr_stream_start` are never called (they
    // would touch the ANE), so both managers stay nil for the whole process and the
    // not-initialized paths are exercised deterministically. No model files, no network.

    /// Transcribing before init returns the not-initialized rc (2), never a crash. The manager
    /// check precedes the sample check, so a nil sample pointer still reaches rc 2.
    func testTranscribeBeforeInitReportsNotInitialized() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_transcribe(nil, 0, 16_000, nil, cb), 2)
    }

    /// Pushing a streaming chunk before start returns the not-started rc (2), never a crash.
    func testStreamPushBeforeStartReportsNotStarted() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_asr_stream_push(nil, 0, 16_000, nil, cb), 2)
    }

    /// Finishing a stream that never started is a graceful empty result (rc 0, a borrowed "" to
    /// the callback), matching the shim's borrowed-empty contract — not a crash. The callback
    /// captures nothing (a `@convention(c)` closure cannot), so only the rc is asserted here.
    func testStreamFinishBeforeStartIsGracefulEmpty() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_asr_stream_finish(nil, cb), 0)
    }

    /// Both ASR shutdowns with no live manager are safe no-ops (idempotent), and a following
    /// transcribe still reports not-initialized rather than touching a freed manager.
    func testAsrShutdownWithoutInitIsSafe() {
        ds_fluid_asr_shutdown()
        ds_fluid_asr_stream_shutdown()
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_transcribe(nil, 0, 16_000, nil, cb), 2)
    }

    // Diarization ABI edges. `ds_fluid_diar_init` is never called (it would touch the ANE), so
    // the manager stays nil and the not-initialized path is exercised deterministically. No
    // model files, no network.

    /// Diarizing before init returns the not-initialized rc (2), never a crash. The manager
    /// check precedes the sample check, so a nil sample pointer still reaches rc 2.
    func testDiarizeBeforeInitReportsNotInitialized() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_diarize(nil, 0, 16_000, nil, cb), 2)
    }

    /// Embedding before init likewise reports not-initialized (2), never a crash.
    func testEmbedBeforeInitReportsNotInitialized() {
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_diar_embed(nil, 0, 16_000, nil, cb), 2)
    }

    /// Shutdown with no live manager is a safe no-op (idempotent), and a following diarize
    /// still reports not-initialized rather than touching a freed manager.
    func testDiarShutdownWithoutInitIsSafe() {
        ds_fluid_diar_shutdown()
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_diarize(nil, 0, 16_000, nil, cb), 2)
    }
}
