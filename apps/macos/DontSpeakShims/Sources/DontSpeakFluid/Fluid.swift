// libdontspeak_fluid -- C ABI over FluidAudio Core ML / ANE (Kokoro TTS, Parakeet ASR,
// pyannote + WeSpeaker diarization). Own dylib so nothing else links FluidAudio. Rust owns
// text frontend + downloads; shim loads offline. See dontspeak_fluid.h.
// 0 = success. Serial helper calls; locks guard shared managers.
import AVFoundation
import FluidAudio
import Foundation

// MARK: - async -> blocking bridge (Rust worker thread only; Swift Task deadlocks the pool)

private final class FluidBox<T>: @unchecked Sendable { var value: Result<T, Error>? }

/// Park calling thread until `op` completes. C entry from Rust worker only.
private func fluidRunBlocking<T>(_ op: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let sem = DispatchSemaphore(value: 0)
    let box = FluidBox<T>()
    Task.detached {
        do { box.value = .success(try await op()) } catch { box.value = .failure(error) }
        sem.signal()
    }
    sem.wait()
    return box.value ?? .failure(FluidShimError.noResult)
}

enum FluidShimError: Error {
    case noResult, nilDir, badAudio
}

// MARK: - borrowed-result callbacks (see dontspeak_shim.h)
// Success: fire cb once with borrowed buffer; Rust copies out. Status is the C return.
public typealias FluidPcmCb =
    @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<Float>?, Int, Int32) -> Void
public typealias FluidStrCb =
    @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

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
    // ABI-reserved; pins KokoroAneComputeUnits.default (ANE RNN + GPU fp32 tail).
    _ = computeUnits
    fluidTts.lock.lock()
    defer { fluidTts.lock.unlock() }
    // Load only; Rust pre-downloads models and G2P/lexicon into FluidAudio's cache path.
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
    _ cb: FluidPcmCb?
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
    // Rust owns G2P so ORT/MLX/Fluid render identical phoneme chunks.
    switch fluidRunBlocking({
        try await mgr.synthesizeFromPhonemesDetailed(p, voice: v, speed: speed)
    }) {
    case .success(let r):
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

// MARK: - ASR (Parakeet TDT v2 English, Core ML / ANE) — fluid STT

private final class FluidAsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: AsrManager?
}
private let fluidAsr = FluidAsrState()

/// Load Parakeet TDT v2 (English-only, mirrors ONNX). 0 = ok.
@_cdecl("ds_fluid_asr_init")
public func ds_fluid_asr_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    // ABI-reserved; loader pins ANE-first defaults.
    _ = computeUnits
    fluidAsr.lock.lock()
    defer { fluidAsr.lock.unlock() }
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> AsrManager in
        // load(from:version:.v2) strips the last path component and re-appends the v2 repo
        // folder (parakeet-tdt-0.6b-v2) — pass that set directory.
        guard let dir else { throw FluidShimError.nilDir }
        let models = try await AsrModels.load(from: dir, version: .v2)
        let mgr = AsrManager(config: .default)
        try await mgr.loadModels(models)
        return mgr
    }) {
    case .success(let mgr):
        fluidAsr.manager = mgr
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_asr_init error: \(e)")
        return 1
    }
}

/// 16 kHz mono f32 → UTF-8 via borrowed cb. Empty → "" (rc 0). Not initialized → rc 2.
@_cdecl("ds_fluid_transcribe")
public func ds_fluid_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidStrCb?
) -> Int32 {
    fluidAsr.lock.lock()
    let mgr = fluidAsr.manager
    fluidAsr.lock.unlock()
    guard let mgr else {
        fluidLogErr("ds_fluid_transcribe: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    switch fluidRunBlocking({ () -> String in
        // Fresh decoder state per utterance (stateless TDT).
        var decoderState = TdtDecoderState.make(decoderLayers: await mgr.decoderLayerCount)
        let result = try await mgr.transcribe(audio, decoderState: &decoderState, language: nil)
        return result.text
    }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_transcribe error: \(e)")
        return 1
    }
}

@_cdecl("ds_fluid_asr_shutdown")
public func ds_fluid_asr_shutdown() {
    fluidAsr.lock.lock()
    let mgr = fluidAsr.manager
    fluidAsr.manager = nil
    fluidAsr.lock.unlock()
    if let mgr {
        _ = fluidRunBlocking({
            await mgr.cleanup()
            return true
        })
    }
}

// MARK: - Streaming ASR (StreamingEouAsrManager)
// start/push/finish like MLX/ONNX streamers. process() is "" mid-stream — partials via
// getPartialTranscript().

private final class FluidStreamAsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: StreamingEouAsrManager?
}
private let fluidStreamAsr = FluidStreamAsrState()

/// Start/reset streaming utterance (modelDir only on first load). 0 = ok.
@_cdecl("ds_fluid_asr_stream_start")
public func ds_fluid_asr_stream_start(_ modelDir: UnsafePointer<CChar>?) -> Int32 {
    fluidStreamAsr.lock.lock()
    defer { fluidStreamAsr.lock.unlock() }
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> StreamingEouAsrManager in
        if let mgr = fluidStreamAsr.manager {
            await mgr.reset()
            return mgr
        }
        guard let dir else { throw FluidShimError.nilDir }
        let mgr = StreamingEouAsrManager(chunkSize: .ms160)  // ~6 partials/sec
        try await mgr.loadModels(from: dir)
        await mgr.reset()
        return mgr
    }) {
    case .success(let mgr):
        fluidStreamAsr.manager = mgr
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_asr_stream_start error: \(e)")
        return 1
    }
}

/// Push chunk; cb gets getPartialTranscript(). Not started → rc 2.
@_cdecl("ds_fluid_asr_stream_push")
public func ds_fluid_asr_stream_push(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidStrCb?
) -> Int32 {
    fluidStreamAsr.lock.lock()
    let mgr = fluidStreamAsr.manager
    fluidStreamAsr.lock.unlock()
    guard let mgr else {
        fluidLogErr("ds_fluid_asr_stream_push: not started")
        return 2
    }
    // Build non-Sendable AVAudioPCMBuffer inside the @Sendable closure (never capture it).
    let audio = samples.map { Array(UnsafeBufferPointer(start: $0, count: n)) } ?? []
    let rate = Double(sampleRate)
    switch fluidRunBlocking({ () -> String in
        guard rate > 0,
            let format = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: rate, channels: 1, interleaved: false),
            let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                frameCapacity: AVAudioFrameCount(max(audio.count, 1)))
        else { throw FluidShimError.badAudio }
        buffer.frameLength = AVAudioFrameCount(audio.count)
        if !audio.isEmpty, let dst = buffer.floatChannelData {
            audio.withUnsafeBufferPointer { dst[0].update(from: $0.baseAddress!, count: audio.count) }
        }
        _ = try await mgr.process(audioBuffer: buffer)
        return await mgr.getPartialTranscript()
    }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_asr_stream_push error: \(e)")
        return 1
    }
}

/// Finish stream; cb borrows final transcript. No live stream → "" (rc 0).
@_cdecl("ds_fluid_asr_stream_finish")
public func ds_fluid_asr_stream_finish(
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidStrCb?
) -> Int32 {
    fluidStreamAsr.lock.lock()
    let mgr = fluidStreamAsr.manager
    fluidStreamAsr.lock.unlock()
    guard let mgr else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    switch fluidRunBlocking({ () -> String in try await mgr.finish() }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_asr_stream_finish error: \(e)")
        return 1
    }
}

/// Drop stream state; batch warm model (ds_fluid_transcribe) stays loaded.
@_cdecl("ds_fluid_asr_stream_shutdown")
public func ds_fluid_asr_stream_shutdown() {
    fluidStreamAsr.lock.lock()
    let mgr = fluidStreamAsr.manager
    fluidStreamAsr.manager = nil
    fluidStreamAsr.lock.unlock()
    if let mgr {
        _ = fluidRunBlocking({
            await mgr.cleanup()
            return true
        })
    }
}

// MARK: - Diarization (pyannote + WeSpeaker Core ML)
// Same JSON as MLX diarizer for shared parse_output. DiarizerManager is not an actor —
// full-call NSLock on every entry (init/diarize/embed). Models offline-loaded.

private final class FluidDiarState: @unchecked Sendable {
    let lock = NSLock()
    var manager: DiarizerManager?
}
private let fluidDiar = FluidDiarState()

/// Load from set dir (two .mlmodelc). clusteringThreshold <= 0 → 0.7. 0 = ok.
@_cdecl("ds_fluid_diar_init")
public func ds_fluid_diar_init(_ modelDir: UnsafePointer<CChar>?, _ clusteringThreshold: Float) -> Int32 {
    fluidDiar.lock.lock()
    defer { fluidDiar.lock.unlock() }
    // debugMode fills speakerDatabase for enrolled-name matching. `let` for @Sendable capture.
    let config: DiarizerConfig = {
        var c =
            clusteringThreshold > 0
            ? DiarizerConfig(clusteringThreshold: clusteringThreshold)
            : DiarizerConfig()
        c.debugMode = true
        return c
    }()
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> DiarizerManager in
        // Basenames must match ds-model coreml_repo DIARIZATION_* (Rust guard cross-checks).
        guard let dir else { throw FluidShimError.nilDir }
        let models = try DiarizerModels.load(
            localSegmentationModel: dir.appendingPathComponent("pyannote_segmentation.mlmodelc"),
            localEmbeddingModel: dir.appendingPathComponent("wespeaker_v2.mlmodelc")
        )
        let mgr = DiarizerManager(config: config)
        mgr.initialize(models: models)
        return mgr
    }) {
    case .success(let mgr):
        fluidDiar.manager = mgr
        return 0
    case .failure(let e):
        fluidLogErr("ds_fluid_diar_init error: \(e)")
        return 1
    }
}

/// 16 kHz mono f32 → JSON {segments, speakers}. Empty → {"segments":[]}. Not init → rc 2.
@_cdecl("ds_fluid_diarize")
public func ds_fluid_diarize(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidStrCb?
) -> Int32 {
    _ = sampleRate  // Caller resamples to 16 kHz mono upstream.
    guard let cb else { return 4 }
    fluidDiar.lock.lock()
    defer { fluidDiar.lock.unlock() }
    guard let mgr = fluidDiar.manager else {
        fluidLogErr("ds_fluid_diarize: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        "{\"segments\":[]}".withCString { cb(ctx, $0) }
        return 0
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    // performCompleteDiarization is synchronous — no async bridge.
    do {
        let result = try mgr.performCompleteDiarization(audio)
        let segs: [[String: Any]] = result.segments.map { seg in
            [
                "speaker": seg.speakerId,
                "start": seg.startTimeSeconds,
                "end": seg.endTimeSeconds,
            ]
        }
        // Embeddings keyed by segment speakerId (engine cluster-join id-space).
        var speakers: [String: [Float]] = [:]
        let db = result.speakerDatabase ?? [:]
        for seg in result.segments {
            let id = seg.speakerId
            if speakers[id] == nil, let emb = db[id] {
                speakers[id] = emb
            }
        }
        let data = try JSONSerialization.data(withJSONObject: [
            "segments": segs,
            "speakers": speakers,
        ])
        String(decoding: data, as: UTF8.self).withCString { cb(ctx, $0) }
        return 0
    } catch {
        fluidLogErr("ds_fluid_diarize error: \(error)")
        return 1
    }
}

/// WeSpeaker embedding for enrollment. Needs diar_init (rc 2). Empty → rc 3.
@_cdecl("ds_fluid_diar_embed")
public func ds_fluid_diar_embed(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidPcmCb?
) -> Int32 {
    _ = sampleRate  // Caller resamples to 16 kHz mono upstream.
    guard let cb else { return 4 }
    fluidDiar.lock.lock()
    defer { fluidDiar.lock.unlock() }
    guard let mgr = fluidDiar.manager else {
        fluidLogErr("ds_fluid_diar_embed: not initialized")
        return 2
    }
    guard let samples, n > 0 else { return 3 }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    do {
        let emb = try mgr.extractSpeakerEmbedding(from: audio)
        emb.withUnsafeBufferPointer { cb(ctx, $0.baseAddress, $0.count, 0) }
        return 0
    } catch {
        fluidLogErr("ds_fluid_diar_embed error: \(error)")
        return 1
    }
}

@_cdecl("ds_fluid_diar_shutdown")
public func ds_fluid_diar_shutdown() {
    fluidDiar.lock.lock()
    let mgr = fluidDiar.manager
    fluidDiar.manager = nil
    fluidDiar.lock.unlock()
    mgr?.cleanup()
}
