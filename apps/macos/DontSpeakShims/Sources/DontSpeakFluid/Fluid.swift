// libdontspeak_fluid -- C ABI over FluidAudio's Core ML / ANE stack: Kokoro TTS (IPA phonemes
// -> 24 kHz mono f32), Parakeet ASR, and pyannote + WeSpeaker diarization. Its own dylib and
// SwiftPM target, so nothing else links FluidAudio. Rust owns the text frontend and
// pre-downloads every model; the shim loads offline (see the offlineMode switch below).
// Namespaced ds_fluid_* so it cannot collide with the other families. See dontspeak_fluid.h.
// 0 = success. Helper calls are serial; a lock guards the shared manager.
import AVFoundation
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
    return box.value ?? .failure(FluidShimError.noResult)
}

enum FluidShimError: Error {
    case noResult, nilDir, badAudio
}

// MARK: - borrowed-result callbacks
// Success path: fire cb once on this thread with a borrowed buffer; Rust copies out -- no
// free. Still blocking via fluidRunBlocking; status is the C return. Mirrors dontspeak_shim.h.
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

// MARK: - ASR (Parakeet TDT v2, English, Core ML / ANE) — the `fluid` STT backend
// Own lock and state, independent of the TTS chain above.

private final class FluidAsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: AsrManager?
}
private let fluidAsr = FluidAsrState()

/// Load Parakeet TDT v2 (English-only; mirrors ONNX — not v3 multilingual). 0 = ok.
@_cdecl("ds_fluid_asr_init")
public func ds_fluid_asr_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    // ABI-reserved to mirror the TTS init; the loader pins the ANE-first defaults this release.
    _ = computeUnits
    fluidAsr.lock.lock()
    defer { fluidAsr.lock.unlock() }
    // offlineMode: load only. DontSpeak pre-downloads the Parakeet set (idempotent — TTS may
    // have set it already, but ASR-only load paths must be offline too).
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> AsrManager in
        // load(from:version:.v2): the loader strips the last path component and re-appends the
        // v2 repo folder (parakeet-tdt-0.6b-v2), so DontSpeak hands it that exact set directory.
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

/// 16 kHz mono f32 → UTF-8 via borrowed cb. Empty input → "" (rc 0). Not initialized → rc 2.
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
// Chunk feed with encoder cache; same start/push/finish shape as the MLX/ONNX streamers.
// process() returns "" mid-stream — use getPartialTranscript() for the live overlay.

private final class FluidStreamAsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: StreamingEouAsrManager?
}
private let fluidStreamAsr = FluidStreamAsrState()

/// Start/reset a streaming utterance (modelDir consulted only on first load). 0 = ok.
@_cdecl("ds_fluid_asr_stream_start")
public func ds_fluid_asr_stream_start(_ modelDir: UnsafePointer<CChar>?) -> Int32 {
    fluidStreamAsr.lock.lock()
    defer { fluidStreamAsr.lock.unlock() }
    ModelHub.offlineMode = true  // DontSpeak pre-downloads the streaming model set
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> StreamingEouAsrManager in
        if let mgr = fluidStreamAsr.manager {
            await mgr.reset()
            return mgr
        }
        guard let dir else { throw FluidShimError.nilDir }
        let mgr = StreamingEouAsrManager(chunkSize: .ms160)  // lowest latency (~6 partials/sec)
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

/// Push a chunk; cb gets getPartialTranscript() (process() returns "" mid-stream). Not
/// started → rc 2.
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
    // Build the non-Sendable AVAudioPCMBuffer INSIDE the @Sendable closure (never capture it).
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
        // process() yields "" until finish/EOU — pull the partial via getPartialTranscript().
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

/// Finish the stream; cb borrows the final transcript. No live stream → "" (rc 0).
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

/// Drop buffered streaming state; the batch warm model (ds_fluid_transcribe) is untouched.
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

// MARK: - Diarization (FluidAudio pyannote + WeSpeaker Core ML) — "who spoke when"
// Own manager+lock; emits the SAME JSON contract as the MLX diarizer
// ({"segments":[...],"speakers":{...}}) so parse_output is shared. DiarizerManager is NOT an
// actor, so a full-call NSLock guards every entry (init, diarize, embed), not just init.
// Models pre-downloaded by DontSpeak; loaded offline via the offlineMode switch in init.

private final class FluidDiarState: @unchecked Sendable {
    let lock = NSLock()
    var manager: DiarizerManager?
}
private let fluidDiar = FluidDiarState()

/// Load the diarizer from a DontSpeak-populated set directory (the directory whose members are
/// the two .mlmodelc bundles). clusteringThreshold <= 0 → FluidAudio's default (0.7; lower =
/// more speakers). 0 = ok.
@_cdecl("ds_fluid_diar_init")
public func ds_fluid_diar_init(_ modelDir: UnsafePointer<CChar>?, _ clusteringThreshold: Float) -> Int32 {
    fluidDiar.lock.lock()
    defer { fluidDiar.lock.unlock() }
    // debugMode → speakerDatabase embeddings for enrolled-name matching. `let` for @Sendable capture.
    let config: DiarizerConfig = {
        var c =
            clusteringThreshold > 0
            ? DiarizerConfig(clusteringThreshold: clusteringThreshold)
            : DiarizerConfig()
        c.debugMode = true
        return c
    }()
    // offlineMode: load only — DontSpeak pre-downloads the diarization set.
    ModelHub.offlineMode = true
    let dir = fluidCString(modelDir).map { URL(fileURLWithPath: $0) }
    switch fluidRunBlocking({ () -> DiarizerManager in
        // The passed dir IS the set directory. CONTRACT: these two basenames match ds-model's
        // coreml_repo DIARIZATION_* consts (a Rust guard cross-checks the literals) — keep literal.
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

/// 16 kHz mono f32 → JSON {segments, speakers}. Same id-space for the engine join. Empty →
/// {"segments":[]}. Not initialized → rc 2.
@_cdecl("ds_fluid_diarize")
public func ds_fluid_diarize(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidStrCb?
) -> Int32 {
    _ = sampleRate  // FluidAudio expects 16 kHz mono; the caller resamples upstream.
    guard let cb else { return 4 }
    // Full-call lock: DiarizerManager is not an actor (races if concurrent).
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
    // performCompleteDiarization is synchronous (throwing) — no async bridge needed.
    do {
        let result = try mgr.performCompleteDiarization(audio)
        let segs: [[String: Any]] = result.segments.map { seg in
            [
                "speaker": seg.speakerId,
                "start": seg.startTimeSeconds,
                "end": seg.endTimeSeconds,
            ]
        }
        // Embeddings keyed by segment speakerId (same id-space as the engine's cluster join).
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

/// WeSpeaker embedding for enrollment. Needs ds_fluid_diar_init (rc 2 otherwise). Empty → rc 3.
@_cdecl("ds_fluid_diar_embed")
public func ds_fluid_diar_embed(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: FluidPcmCb?
) -> Int32 {
    _ = sampleRate  // FluidAudio expects 16 kHz mono; the caller resamples upstream.
    guard let cb else { return 4 }
    // Full-call lock (non-actor manager).
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
        // Borrow the embedding to the callback (sample_rate is irrelevant for an embedding).
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
