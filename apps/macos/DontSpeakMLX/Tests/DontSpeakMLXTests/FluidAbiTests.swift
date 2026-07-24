import XCTest

@testable import DontSpeakMLX

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
        let cb: MlxPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }

    /// Shutdown with no live manager is a safe no-op (idempotent), and a following synthesize
    /// still reports not-initialized rather than touching a freed manager.
    func testShutdownWithoutInitIsSafe() {
        ds_fluid_tts_shutdown()
        let cb: MlxPcmCb = { _, _, _, _ in }
        XCTAssertEqual(ds_fluid_tts_synthesize_phonemes(nil, nil, 1.0, nil, cb), 2)
    }
}
