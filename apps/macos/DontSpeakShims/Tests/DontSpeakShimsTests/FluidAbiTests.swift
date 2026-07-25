import XCTest

@testable import DontSpeakFluid

/// FluidAudio TTS/ASR/diar ABI edges only — no model files, ANE, or network.
/// Init/stream_start never called (would touch ANE); managers stay nil so
/// not-initialized paths are deterministic.
final class FluidAbiTests: XCTestCase {
    /// Missing cb must fail before model work (success promises exactly one callback).
    func testSynthesisRejectsNullCallback() {
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, nil), 4)
    }

    /// Before init → rc 2. Manager check precedes phoneme check (nil phoneme still rc 2).
    func testSynthesisBeforeInitReportsNotInitialized() {
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }

    /// Shutdown with no manager is idempotent; following synthesize still rc 2.
    func testShutdownWithoutInitIsSafe() {
        ds_fluid_tts_shutdown()
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }

    // ASR edges. asr_init / stream_start never called.

    /// Before init → rc 2. Manager check precedes sample check.
    func testTranscribeBeforeInitReportsNotInitialized() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_transcribe(nil, 0, 16_000, nil, cb), 2)
    }

    /// Stream push before start → rc 2.
    func testStreamPushBeforeStartReportsNotStarted() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_asr_stream_push(nil, 0, 16_000, nil, cb), 2)
    }

    /// Finish never-started → rc 0 with borrowed "" (borrowed-empty contract).
    func testStreamFinishBeforeStartIsGracefulEmpty() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_asr_stream_finish(nil, cb), 0)
    }

    /// Both ASR shutdowns idempotent; following transcribe still rc 2.
    func testAsrShutdownWithoutInitIsSafe() {
        ds_fluid_asr_shutdown()
        ds_fluid_asr_stream_shutdown()
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_transcribe(nil, 0, 16_000, nil, cb), 2)
    }

    // Diarization edges. diar_init never called.

    /// Before init → rc 2. Manager check precedes sample check.
    func testDiarizeBeforeInitReportsNotInitialized() {
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_diarize(nil, 0, 16_000, nil, cb), 2)
    }

    /// Embed before init → rc 2.
    func testEmbedBeforeInitReportsNotInitialized() {
        let cb: FluidPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_diar_embed(nil, 0, 16_000, nil, cb), 2)
    }

    /// Shutdown with no manager is idempotent; following diarize still rc 2.
    func testDiarShutdownWithoutInitIsSafe() {
        ds_fluid_diar_shutdown()
        let cb: FluidStrCb = { _, _ in }
        XCTAssertEqual(ds_fluid_diarize(nil, 0, 16_000, nil, cb), 2)
    }
}
