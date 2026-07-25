// libdontspeak_mlx -- C ABI over MLX Audio (all built-in MLX TTS, Parakeet STT, Sortformer
// diarization). Rust owns the text frontend and model downloads. See dontspeak_mlx.h.
// 0 = success. Helper calls are serial; locks guard shared models.
import AVFoundation
import Foundation
@preconcurrency import MLX
import MLXAudioSTT
import MLXAudioTTS
import MLXAudioVAD

// MARK: - async → blocking bridge (called from a Rust worker thread)

private final class Box<T>: @unchecked Sendable { var value: Result<T, Error>? }
private final class SendableValue<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}

/// Park calling thread until `op` completes. C entry from Rust worker only —
/// never from a Swift Task (cooperative-pool deadlock risk).
private func runBlocking<T>(_ op: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let sem = DispatchSemaphore(value: 0)
    let box = Box<T>()
    Task.detached {
        do { box.value = .success(try await op()) } catch { box.value = .failure(error) }
        sem.signal()
    }
    sem.wait()
    return box.value ?? .failure(MlxShimError.noResult)
}

enum MlxShimError: Error {
    case noResult, nilDir, badAudio
}

// MARK: - borrowed-result callbacks
// Success path: fire cb once on this thread with borrowed buffer; Rust copies out — no free.
// Still blocking via runBlocking; status is the C return. Types mirror dontspeak_mlx.h.
public typealias MlxPcmCb =
    @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<Float>?, Int, Int32) -> Void
public typealias MlxStrCb = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

// MARK: - shared state

private struct PassthroughProcessor: TextProcessor {
    func process(text: String, language: String?) throws -> String { text }
}
private enum TtsKind: String {
    case kokoro, chatterbox, qwen, omnivoice

    var usesManagedHubCache: Bool { self == .chatterbox || self == .omnivoice }
}

/// Rust TTS-param registry mirror; values say whether the pinned MLX API applies a key.
/// Registry edits must update this map and the Rust/Swift literal drift tests together.
let ttsParamMirror: [String: [String: Bool]] = [
    "kokoro": [:],
    "chatterbox": ["exaggeration": true],
    "qwen": ["repetition_penalty": true],
    "omnivoice": ["steps": true, "seed": true],
]

struct TtsAppliedParams: Equatable, Sendable {
    var chatterboxExaggeration: Float?
    var qwenRepetitionPenalty: Float?
    var omniVoiceSteps: Int?
    var omniVoiceSeed: Int64?
}

struct TtsParamDecode: Equatable {
    var applied: [String] = []
    var ignored: [String] = []
    var unknown: [String] = []
    var values = TtsAppliedParams()
}

private func finiteFloat(_ value: Any) -> Float? {
    guard !(value is Bool), let number = value as? NSNumber else { return nil }
    let decoded = number.floatValue
    return decoded.isFinite ? decoded : nil
}

private func integer(_ value: Any) -> Int64? {
    guard !(value is Bool), let number = value as? NSNumber else { return nil }
    let encoding = String(cString: number.objCType)
    guard encoding != "f", encoding != "d" else { return nil }
    return number.int64Value
}

private func decodeTtsParam(
    model: String, key: String, value: Any, into params: inout TtsAppliedParams
) -> Bool {
    switch (model, key) {
    case ("chatterbox", "exaggeration"):
        guard let value = finiteFloat(value), (0.25...2.0).contains(value) else { return false }
        params.chatterboxExaggeration = value
    case ("qwen", "repetition_penalty"):
        guard let value = finiteFloat(value), (1.0...3.0).contains(value) else { return false }
        params.qwenRepetitionPenalty = value
    case ("omnivoice", "steps"):
        guard let value = integer(value), (1...64).contains(value), let value = Int(exactly: value)
        else { return false }
        params.omniVoiceSteps = value
    case ("omnivoice", "seed"):
        guard let value = integer(value), value >= -1 else { return false }
        params.omniVoiceSeed = value
    default:
        return false
    }
    return true
}

/// Sorted classification; malformed JSON returns nil without failing synthesis.
func ttsParamsDecode(model: String, json: String) -> TtsParamDecode? {
    guard let data = json.data(using: .utf8),
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { return nil }
    let mirror = ttsParamMirror[model] ?? [:]
    var decode = TtsParamDecode()
    for key in object.keys.sorted() {
        switch mirror[key] {
        case .some(true):
            guard decodeTtsParam(model: model, key: key, value: object[key]!, into: &decode.values)
            else { return nil }
            decode.applied.append(key)
        case .some(false): decode.ignored.append(key)
        case .none: decode.unknown.append(key)
        }
    }
    return decode
}

/// FNV-1a with NUL-separated fields, matching the ORT OmniVoice backend.
func stableOmniVoiceSeed(language: String, instruct: String) -> UInt64 {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for bytes in [Array(language.utf8), [0], Array(instruct.utf8)] {
        for byte in bytes {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
    }
    return hash
}
private final class ShimState: @unchecked Sendable {
    let lock = NSLock()
    var tts: (any SpeechGenerationModel)?
    var ttsKind: TtsKind?
}
private let state = ShimState()

private func cString(_ p: UnsafePointer<CChar>?) -> String? {
    guard let p else { return nil }
    let s = String(cString: p)
    return s.isEmpty ? nil : s
}

/// MLX Audio's OmniVoice loader currently exposes only its repository API. Point its
/// cache at the already verified DontSpeak directory; ModelUtils returns that complete
/// local snapshot before any network operation. Chatterbox shares the root for its S3
/// tokenizer, keeping every native model asset on the Rust-managed download path.
private func configureManagedHubCache(for modelDir: URL) -> Bool {
    let mlxAudioDir = modelDir.deletingLastPathComponent()
    guard mlxAudioDir.lastPathComponent == "mlx-audio" else {
        logErr("managed MLX model is outside the mlx-audio cache layout: \(modelDir.path)")
        return false
    }
    let cacheRoot = mlxAudioDir.deletingLastPathComponent()
    return setenv("HF_HUB_CACHE", cacheRoot.path, 1) == 0
}

// MARK: - C ABI

@_cdecl("ds_mlx_tts_init")
public func ds_mlx_tts_init(
    _ modelName: UnsafePointer<CChar>?,
    _ modelDir: UnsafePointer<CChar>?
) -> Int32 {
    guard let name = cString(modelName) else { return 3 }
    guard let path = cString(modelDir) else { return 3 }
    guard let kind = TtsKind(rawValue: name) else { return 3 }
    configureMlxMemoryPolicy()
    state.lock.lock()
    defer { state.lock.unlock() }
    let dir = URL(fileURLWithPath: path)
    state.tts = nil
    state.ttsKind = nil
    guard !kind.usesManagedHubCache || configureManagedHubCache(for: dir) else { return 3 }
    let loaded: Result<any SpeechGenerationModel, Error> = switch kind {
    case .kokoro:
        runBlocking {
            try await KokoroModel.fromModelDirectory(
                dir, textProcessor: PassthroughProcessor()) as any SpeechGenerationModel
        }
    case .chatterbox:
        runBlocking {
            try await ChatterboxModel.fromModelDirectory(
                dir, hfToken: nil) as any SpeechGenerationModel
        }
    case .qwen:
        runBlocking {
            try await Qwen3TTSModel.fromModelDirectory(dir) as any SpeechGenerationModel
        }
    case .omnivoice:
        runBlocking {
            try await OmniVoiceModel.fromPretrained(
                "mlx-community/OmniVoice-bf16") as any SpeechGenerationModel
        }
    }
    switch loaded {
    case .success(let model):
        state.tts = model
        state.ttsKind = kind
        logMlxMemory(phase: "tts_init")
        return 0
    case .failure(let error):
        logErr("ds_mlx_tts_init \(name) error: \(error)")
        return 1
    }
}

// Versioned call ABI: skew must fail symbol lookup before arguments are passed.
@_cdecl("ds_mlx_tts_synthesize2")
public func ds_mlx_tts_synthesize2(
    _ text: UnsafePointer<CChar>?,
    _ voice: UnsafePointer<CChar>?,
    _ language: UnsafePointer<CChar>?,
    _ speed: Float,
    _ paramsJson: UnsafePointer<CChar>?,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxPcmCb?
) -> Int32 {
    guard let cb else { return 4 }
    guard let text = cString(text) else { return 3 }
    let v = cString(voice)
    let language = cString(language)
    state.lock.lock()
    guard let model = state.tts, let kind = state.ttsKind else {
        state.lock.unlock()
        logErr("ds_mlx_tts_synthesize2: not initialized")
        return 2
    }
    var appliedParams = TtsAppliedParams()
    // Settings are advisory; only unknown or malformed input is logged.
    if let paramsJson = cString(paramsJson) {
        if let decode = ttsParamsDecode(model: kind.rawValue, json: paramsJson) {
            appliedParams = decode.values
            if !decode.unknown.isEmpty {
                logErr(
                    "ds_mlx_tts_synthesize2: unknown params ignored: "
                        + decode.unknown.joined(separator: ","))
            }
        } else {
            logErr("ds_mlx_tts_synthesize2: malformed params_json ignored")
        }
    }
    if kind == .kokoro, let kokoro = model as? KokoroModel {
        kokoro.speed = speed.isFinite ? max(0.25, min(speed, 4.0)) : 1.0
    }
    if kind == .chatterbox, let chatterbox = model as? ChatterboxModel,
        let exaggeration = appliedParams.chatterboxExaggeration
    {
        chatterbox.emotionAdvOverride = exaggeration
        // mlx-audio-swift 0.1.3 only consults the override while preparing reference
        // conditioning. DontSpeak uses the bundled default conditioning, so update it too.
        chatterbox.defaultConditioning?.emotionAdv = MLXArray(exaggeration)
    }
    let selectedVoice: String? = switch kind {
    case .chatterbox: nil
    case .omnivoice: v == "default" ? nil : v
    case .kokoro, .qwen: v
    }
    let synthesisParams = appliedParams
    let sendableModel = SendableValue(model)
    let result: Result<(MLXArray, Int), Error> = runBlocking {
        let model = sendableModel.value
        let audio: MLXArray
        if kind == .omnivoice, let omniVoice = model as? OmniVoiceModel {
            var parameters = OmniVoiceGenerateParameters()
            if let steps = synthesisParams.omniVoiceSteps {
                parameters.numStep = steps
            }
            if let configuredSeed = synthesisParams.omniVoiceSeed {
                let seed = configuredSeed >= 0
                    ? UInt64(configuredSeed)
                    : stableOmniVoiceSeed(language: language ?? "", instruct: selectedVoice ?? "")
                MLXRandom.seed(seed)
            }
            audio = try await omniVoice.generate(
                text: text,
                voice: selectedVoice,
                refAudio: nil,
                refText: nil,
                language: language,
                ovParameters: parameters)
        } else {
            var generationParameters = model.defaultGenerationParameters
            if let repetitionPenalty = synthesisParams.qwenRepetitionPenalty {
                generationParameters.repetitionPenalty = repetitionPenalty
            }
            audio = try await model.generate(
                text: text,
                voice: selectedVoice,
                refAudio: nil,
                refText: nil,
                language: language,
                generationParameters: generationParameters)
        }
        return (audio, model.sampleRate)
    }
    state.lock.unlock()
    switch result {
    case .success(let (audio, sampleRate)):
        let samples = audio.asArray(Float.self)
        samples.withUnsafeBufferPointer {
            cb(ctx, $0.baseAddress, $0.count, Int32(sampleRate))
        }
        logMlxMemory(phase: "tts_synthesize")
        return 0
    case .failure(let error):
        logErr("ds_mlx_tts_synthesize2 error: \(error)")
        logMlxMemory(phase: "tts_synthesize_error")
        return 1
    }
}

@_cdecl("ds_mlx_tts_shutdown")
public func ds_mlx_tts_shutdown() {
    state.lock.lock()
    state.tts = nil
    state.ttsKind = nil
    state.lock.unlock()
    clearMlxCacheAndLog(phase: "tts_shutdown")
}

// MARK: - ASR (Parakeet TDT v3, multilingual, MLX)

private final class AsrState: @unchecked Sendable {
    let lock = NSLock()
    var model: ParakeetModel?
    var streamSamples: [Float] = []
    var streamSamplesAtLastDecode = 0
    var streamText = ""
}
private let asr = AsrState()

let asrPartialMinimumSamples = 16_000
let asrPartialDecodeIntervalSamples = 16_000

func asrShouldDecodePartial(totalSamples: Int, lastDecodedSamples: Int) -> Bool {
    totalSamples >= asrPartialMinimumSamples
        && totalSamples - lastDecodedSamples >= asrPartialDecodeIntervalSamples
}

private func loadParakeet(_ modelDir: UnsafePointer<CChar>?) throws -> ParakeetModel {
    guard let path = cString(modelDir) else { throw MlxShimError.nilDir }
    return try ParakeetModel.fromDirectory(URL(fileURLWithPath: path))
}

private func transcribe(_ model: ParakeetModel, _ samples: [Float]) -> String {
    model.generate(audio: MLXArray(samples)).text
}

/// Load Parakeet TDT v3 multilingual, matching the ONNX path. 0 = ok.
@_cdecl("ds_mlx_asr_init")
public func ds_mlx_asr_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    _ = computeUnits
    configureMlxMemoryPolicy()
    asr.lock.lock()
    defer { asr.lock.unlock() }
    do {
        asr.model = try loadParakeet(modelDir)
        logMlxMemory(phase: "asr_init")
        return 0
    } catch {
        logErr("ds_mlx_asr_init error: \(error)")
        return 1
    }
}

/// 16 kHz mono f32 → UTF-8 via borrowed cb. Empty input → "" (rc 0).
@_cdecl("ds_mlx_transcribe")
public func ds_mlx_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxStrCb?
) -> Int32 {
    guard let cb else { return 4 }
    asr.lock.lock()
    guard let model = asr.model else {
        asr.lock.unlock()
        logErr("ds_mlx_transcribe: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        asr.lock.unlock()
        "".withCString { cb(ctx, $0) }
        return 0
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    let text = transcribe(model, audio)
    asr.lock.unlock()
    text.withCString { cb(ctx, $0) }
    logMlxMemory(phase: "asr_transcribe")
    return 0
}

@_cdecl("ds_mlx_asr_shutdown")
public func ds_mlx_asr_shutdown() {
    asr.lock.lock()
    asr.model = nil
    asr.streamSamples.removeAll(keepingCapacity: false)
    asr.streamSamplesAtLastDecode = 0
    asr.streamText = ""
    asr.lock.unlock()
    clearMlxCacheAndLog(phase: "asr_shutdown")
}

/// End buffered streaming state without unloading the shared warm Parakeet model.
@_cdecl("ds_mlx_asr_stream_shutdown")
public func ds_mlx_asr_stream_shutdown() {
    asr.lock.lock()
    asr.streamSamples.removeAll(keepingCapacity: false)
    asr.streamSamplesAtLastDecode = 0
    asr.streamText = ""
    asr.lock.unlock()
}

// MARK: - Buffered streaming ASR
// MLX Audio's Parakeet API consumes a complete array. Buffer chunks, periodically re-decode
// the utterance for live partials, then run one final decode at finish.

/// Start/reset streaming utterance (modelDir only on first load). 0 = ok.
@_cdecl("ds_mlx_asr_stream_start")
public func ds_mlx_asr_stream_start(_ modelDir: UnsafePointer<CChar>?) -> Int32 {
    configureMlxMemoryPolicy()
    asr.lock.lock()
    defer { asr.lock.unlock() }
    do {
        if asr.model == nil { asr.model = try loadParakeet(modelDir) }
        asr.streamSamples.removeAll(keepingCapacity: true)
        asr.streamSamplesAtLastDecode = 0
        asr.streamText = ""
        logMlxMemory(phase: "asr_stream_start")
        return 0
    } catch {
        logErr("ds_mlx_asr_stream_start error: \(error)")
        return 1
    }
}

/// Push chunk; at most once per second of new 16 kHz audio, cb receives a refreshed hypothesis.
@_cdecl("ds_mlx_asr_stream_push")
public func ds_mlx_asr_stream_push(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxStrCb?
) -> Int32 {
    _ = sampleRate
    guard let cb else { return 4 }
    asr.lock.lock()
    guard let model = asr.model else {
        asr.lock.unlock()
        logErr("ds_mlx_asr_stream_push: not started")
        return 2
    }
    if let samples, n > 0 {
        asr.streamSamples.append(contentsOf: UnsafeBufferPointer(start: samples, count: n))
    }
    if asrShouldDecodePartial(
        totalSamples: asr.streamSamples.count,
        lastDecodedSamples: asr.streamSamplesAtLastDecode
    ) {
        asr.streamText = transcribe(model, asr.streamSamples)
        asr.streamSamplesAtLastDecode = asr.streamSamples.count
    }
    let text = asr.streamText
    asr.lock.unlock()
    text.withCString { cb(ctx, $0) }
    return 0
}

/// Finish stream; cb borrows final transcript.
@_cdecl("ds_mlx_asr_stream_finish")
public func ds_mlx_asr_stream_finish(
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxStrCb?
) -> Int32 {
    guard let cb else { return 4 }
    asr.lock.lock()
    guard let model = asr.model else {
        asr.lock.unlock()
        "".withCString { cb(ctx, $0) }
        return 0
    }
    if !asr.streamSamples.isEmpty {
        asr.streamText = transcribe(model, asr.streamSamples)
    }
    let text = asr.streamText
    asr.streamSamples.removeAll(keepingCapacity: true)
    asr.streamSamplesAtLastDecode = 0
    asr.lock.unlock()
    text.withCString { cb(ctx, $0) }
    logMlxMemory(phase: "asr_stream_finish")
    return 0
}

// MARK: - Diarization (Sortformer + WeSpeaker MLX)
// Own manager+lock; JSON out for C ABI. Models pre-downloaded by DontSpeak.

private final class DiarState: @unchecked Sendable {
    let lock = NSLock()
    var model: SortformerModel?
    var encoder: SpeakerEncoder?
    var threshold: Float = 0.5
}
private let diar = DiarState()

struct LabeledSampleRange {
    let speaker: String
    let start: Int
    let end: Int
}

func exclusiveRanges(
    for speaker: String, in ranges: [LabeledSampleRange]
) -> [Range<Int>] {
    let blockers = ranges.filter { $0.speaker != speaker && $0.start < $0.end }
    return ranges.filter { $0.speaker == speaker && $0.start < $0.end }.flatMap { own in
        blockers.reduce([own.start..<own.end]) { fragments, blocker in
            fragments.flatMap { fragment in
                guard blocker.start < fragment.upperBound, blocker.end > fragment.lowerBound else {
                    return [fragment]
                }
                var remaining: [Range<Int>] = []
                if fragment.lowerBound < blocker.start {
                    remaining.append(fragment.lowerBound..<min(fragment.upperBound, blocker.start))
                }
                if blocker.end < fragment.upperBound {
                    remaining.append(max(fragment.lowerBound, blocker.end)..<fragment.upperBound)
                }
                return remaining
            }
        }
    }
}

/// Load diarizer and publish both models atomically after successful initialization.
@_cdecl("ds_mlx_diar_init")
public func ds_mlx_diar_init(_ modelDir: UnsafePointer<CChar>?, _ activityThreshold: Float) -> Int32 {
    guard let path = cString(modelDir) else { return 3 }
    configureMlxMemoryPolicy()
    let root = URL(fileURLWithPath: path)
    do {
        let model = try SortformerModel.fromModelDirectory(
            root.appendingPathComponent("sortformer"))
        let encoder = try SpeakerEncoder.load(
            from: root.appendingPathComponent("wespeaker"))
        let threshold =
            activityThreshold > 0
            ? min(max(activityThreshold, 0.1), 0.9) : 0.5
        diar.lock.lock()
        diar.model = model
        diar.encoder = encoder
        diar.threshold = threshold
        diar.lock.unlock()
        logMlxMemory(phase: "diar_init")
        return 0
    } catch {
        logErr("ds_mlx_diar_init error: \(error)")
        return 1
    }
}

/// 16 kHz mono f32 → JSON {segments, speakers}. Same id-space for join. Empty → {"segments":[]}.
@_cdecl("ds_mlx_diarize")
public func ds_mlx_diarize(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxStrCb?
) -> Int32 {
    _ = sampleRate
    guard let cb else { return 4 }
    diar.lock.lock()
    guard let model = diar.model, let encoder = diar.encoder else {
        diar.lock.unlock()
        logErr("ds_mlx_diarize: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        diar.lock.unlock()
        "{\"segments\":[]}".withCString { cb(ctx, $0) }
        return 0
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    let modelBox = SendableValue(model)
    let threshold = diar.threshold
    switch runBlocking({
        try await modelBox.value.generate(
            audio: MLXArray(audio), sampleRate: 16_000,
            threshold: threshold, minDuration: 0.2, mergeGap: 0.15)
    }) {
    case .success(let result):
        let segs: [[String: Any]] = result.segments.map { seg in
            [
                "speaker": String(seg.speaker),
                "start": seg.start,
                "end": seg.end,
            ]
        }
        let ranges = result.segments.map { segment in
            LabeledSampleRange(
                speaker: String(segment.speaker),
                start: min(audio.count, max(0, Int(segment.start * 16_000))),
                end: min(audio.count, max(0, Int(segment.end * 16_000))))
        }
        var speakers: [String: [Float]] = [:]
        for speaker in Set(result.segments.map(\.speaker)) {
            var speakerAudio: [Float] = []
            for range in exclusiveRanges(for: String(speaker), in: ranges) {
                speakerAudio.append(contentsOf: audio[range])
            }
            if let embedding = try? encoder.embed(speakerAudio) {
                speakers[String(speaker)] = embedding
            }
        }
        do {
            let data = try JSONSerialization.data(withJSONObject: [
                "segments": segs,
                "speakers": speakers,
            ])
            let json = String(decoding: data, as: UTF8.self)
            diar.lock.unlock()
            json.withCString { cb(ctx, $0) }
            logMlxMemory(phase: "diarize")
            return 0
        } catch {
            diar.lock.unlock()
            logErr("ds_mlx_diarize JSON error: \(error)")
            logMlxMemory(phase: "diarize_error")
            return 1
        }
    case .failure(let error):
        diar.lock.unlock()
        logErr("ds_mlx_diarize error: \(error)")
        logMlxMemory(phase: "diarize_error")
        return 1
    }
}

@_cdecl("ds_mlx_diar_shutdown")
public func ds_mlx_diar_shutdown() {
    diar.lock.lock()
    diar.model = nil
    diar.encoder = nil
    diar.lock.unlock()
    clearMlxCacheAndLog(phase: "diar_shutdown")
}

/// WeSpeaker embedding for enrollment. Needs `ds_mlx_diar_init`. Empty → rc 3.
@_cdecl("ds_mlx_diar_embed")
public func ds_mlx_diar_embed(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: MlxPcmCb?
) -> Int32 {
    _ = sampleRate
    guard let cb else { return 4 }
    diar.lock.lock()
    guard let encoder = diar.encoder else {
        diar.lock.unlock()
        logErr("ds_mlx_diar_embed: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        diar.lock.unlock()
        return 3
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    do {
        let emb = try encoder.embed(audio)
        diar.lock.unlock()
        emb.withUnsafeBufferPointer { cb(ctx, $0.baseAddress, $0.count, 0) }
        logMlxMemory(phase: "diar_embed")
        return 0
    } catch {
        diar.lock.unlock()
        logErr("ds_mlx_diar_embed error: \(error)")
        logMlxMemory(phase: "diar_embed_error")
        return 1
    }
}
