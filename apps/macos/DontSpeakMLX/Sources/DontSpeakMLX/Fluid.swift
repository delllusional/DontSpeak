// Fluid.swift -- C ABI over FluidAudio's ANE Kokoro TTS (IPA phonemes -> 24 kHz mono f32).
// A SECOND source in DontSpeakMLX, never referenced by shim.swift, so the Intel compatibility
// build (shim.swift alone, no SwiftPM) never links FluidAudio. Rust owns the text frontend and
// pre-downloads every model; the shim loads offline (see the offlineMode switch below).
// Namespaced ds_fluid_* so it cannot collide with shim.swift's ds_mlx_*. See dontspeak_mlx.h.
// Reuses shim.swift's public MlxPcmCb and internal MlxShimError; its own private helpers so
// nothing here depends on shim.swift symbols. 0 = success. Helper calls are serial; a lock
// guards the shared manager.
import FluidAudio
import Foundation

// MARK: - async -> blocking bridge (called from a Rust worker thread, never a Swift Task)

private final class FluidBox<T>: @unchecked Sendable { var value: Result<T, Error>? }

/// Park the calling thread until `op` completes. C entry from a Rust worker only --
/// never from a Swift Task (cooperative-pool deadlock risk).
private func fluidRunBlocking<T>(_ op: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let sem = DispatchSemaphore(value: 0)
    let box = FluidBox<T>()
    Task.detached {
        do { box.value = .success(try await op()) } catch { box.value = .failure(error) }
        sem.signal()
    }
    sem.wait()
    return box.value ?? .failure(MlxShimError.noResult)
}

private func fluidLogErr(_ s: String) {
    FileHandle.standardError.write(Data((s + "\n").utf8))
}

private func fluidCString(_ p: UnsafePointer<CChar>?) -> String? {
    guard let p else { return nil }
    let s = String(cString: p)
    return s.isEmpty ? nil : s
}

private final class FluidTtsState: @unchecked Sendable {
    let lock = NSLock()
    var manager: KokoroAneManager?
}
private let fluidTts = FluidTtsState()

// MARK: - C ABI

@_cdecl("ds_fluid_tts_init")
public func ds_fluid_tts_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    // ABI-reserved to mirror the deleted shim; this slice pins the recommended `.default`
    // preset (ANE-resident RNN stages + GPU fp32 tail).
    _ = computeUnits
    fluidTts.lock.lock()
    defer { fluidTts.lock.unlock() }
    // offlineMode: load only. DontSpeak pre-downloads (integrity + % UI) and pre-fills the
    // G2P/lexicon set `initialize()` ensures from FluidAudio's own hardcoded cache path.
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    let mgr = KokoroAneManager(
        variant: .english,
        defaultVoice: nil,
        directory: dir,
        computeUnits: KokoroAneComputeUnits(preset: .default)
    )
    switch fluidRunBlocking({ try await mgr.initialize() }) {
    case .success:
        fluidTts.manager = mgr
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_tts_init error: \(e)")
        return 1
    }
}

@_cdecl("ds_fluid_tts_synthesize_phonemes")
public func ds_fluid_tts_synthesize_phonemes(
    _ phonemes: UnsafePointer<CChar>?,
    _ voice: UnsafePointer<CChar>?,
    _ speed: Float,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxPcmCb?
) -> Int32 {
    guard let cb else { return 4 }
    fluidTts.lock.lock()
    let mgr = fluidTts.manager
    fluidTts.lock.unlock()
    guard let mgr else {
        fluidLogErr("ds_fluid_tts_synthesize_phonemes: not initialized")
        return 2
    }
    guard let p = fluidCString(phonemes) else { return 3 }
    let v = fluidCString(voice)
    // Rust owns G2P; this skips FluidAudio's own G2P so ORT/MLX/Fluid receive identical phonemes.
    switch fluidRunBlocking({
        try await mgr.synthesizeFromPhonemesDetailed(p, voice: v, speed: speed)
    }) {
    case .success(let r):
        // Borrow the samples to the callback (it copies them out); no ownership transfer.
        r.samples.withUnsafeBufferPointer { cb(ctx, $0.baseAddress, $0.count, Int32(r.sampleRate)) }
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_tts_synthesize_phonemes error: \(e)")
        return 1
    }
}

@_cdecl("ds_fluid_tts_shutdown")
public func ds_fluid_tts_shutdown() {
    fluidTts.lock.lock()
    let mgr = fluidTts.manager
    fluidTts.manager = nil
    fluidTts.lock.unlock()
    if let mgr {
        _ = fluidRunBlocking({
            await mgr.cleanup()
            return true
        })
    }
}
