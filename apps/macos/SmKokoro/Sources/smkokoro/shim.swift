// libsmkokoro — a thin C ABI over FluidAudio's ANE Kokoro, dlopen'd by the
// DontSpeak Rust helper. Text in, 24 kHz mono fp32 PCM out. No tokens, no vocab
// matching — FluidAudio's own Core ML G2P handles phonemization.
//
// C ABI (see smkokoro.h):
//   int32 smk_init(const char* model_dir, int32 compute_units)
//   int32 smk_synthesize_text(const char* text, const char* voice, float speed,
//                             float** out_pcm, size_t* out_len, int32* out_sample_rate)
//   void  smk_free(float* pcm)
//   void  smk_shutdown(void)
//
// Returns 0 on success, non-zero on error. The helper drives synthesis serially;
// a lock guards the shared manager for safety.
import AVFoundation
import FluidAudio
import Foundation
import Speech

// MARK: - async → blocking bridge (called from a Rust worker thread)

private final class Box<T>: @unchecked Sendable { var value: Result<T, Error>? }

/// Parks the CALLING thread on a semaphore until `op` completes. Only safe because every
/// caller is a C entry point invoked from a Rust worker thread — NEVER call this from a
/// Swift concurrency task: blocking a cooperative-pool thread on work that needs the same
/// pool can deadlock.
private func runBlocking<T>(_ op: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let sem = DispatchSemaphore(value: 0)
    let box = Box<T>()
    Task.detached {
        do { box.value = .success(try await op()) } catch { box.value = .failure(error) }
        sem.signal()
    }
    sem.wait()
    return box.value ?? .failure(SmkError.noResult)
}

private enum SmkError: Error {
    case noResult, notInitialized, nilText, nilDir, badAudio
    case sysUnavailable(String)
}

// MARK: - borrowed-result callbacks
//
// Buffer-returning calls still BLOCK (via `runBlocking`) and still return their status code as
// the C return value. What changed is result delivery: instead of allocating an owned buffer
// the caller must free (`smk_free`/`smk_free_str`), they BORROW the result to one of these
// callbacks — fired once, synchronously, on this same thread before the function returns. The
// Rust side copies it out during the call, so there is no ownership transfer and nothing to
// free. The callback runs only on the success (rc 0) path. These types mirror smkokoro.h.
public typealias SmkPcmCb =
    @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<Float>?, Int, Int32) -> Void
public typealias SmkStrCb = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

// MARK: - shared state

private final class ShimState: @unchecked Sendable {
    let lock = NSLock()
    var manager: KokoroAneManager?
}
private let state = ShimState()

private func preset(_ i: Int32) -> TtsComputeUnitPreset {
    switch i {
    case 1: return .allAne  // every stage on the Neural Engine
    case 2: return .cpuAndGpu  // skip the ANE (GPU)
    case 3: return .cpuOnly
    case 4: return .aneTailGpu
    default: return .default  // ANE-resident RNN stages + GPU fp32 tail (recommended)
    }
}

private func cString(_ p: UnsafePointer<CChar>?) -> String? {
    guard let p else { return nil }
    let s = String(cString: p)
    return s.isEmpty ? nil : s
}

private func logErr(_ s: String) {
    FileHandle.standardError.write(Data((s + "\n").utf8))
}

// MARK: - C ABI

@_cdecl("smk_init")
public func smk_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    state.lock.lock()
    defer { state.lock.unlock() }
    // DontSpeak pre-downloads EVERY FluidAudio model itself (so it owns integrity + shows real
    // %); FluidAudio must only LOAD from the dirs we populated, never fetch. enforceOffline
    // turns any gap into a typed `modelMissing` instead of a silent download.
    DownloadUtils.enforceOffline = true
    let dir = cString(modelDir).map { URL(fileURLWithPath: $0) }
    let mgr = KokoroAneManager(
        variant: .english,
        defaultVoice: nil,
        directory: dir,
        computeUnits: KokoroAneComputeUnits(preset: preset(computeUnits))
    )
    switch runBlocking({ try await mgr.initialize() }) {
    case .success:
        state.manager = mgr
        return 0
    case .failure(let e):
        logErr("smk_init error: \(e)")
        return 1
    }
}

@_cdecl("smk_synthesize_text")
public func smk_synthesize_text(
    _ text: UnsafePointer<CChar>?,
    _ voice: UnsafePointer<CChar>?,
    _ speed: Float,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkPcmCb?
) -> Int32 {
    state.lock.lock()
    let mgr = state.manager
    state.lock.unlock()
    guard let mgr else {
        logErr("smk_synthesize: not initialized")
        return 2
    }
    guard let t = cString(text) else { return 3 }
    let v = cString(voice)
    switch runBlocking({ try await mgr.synthesizeDetailed(text: t, voice: v, speed: speed) }) {
    case .success(let r):
        // Borrow the samples to the callback (it copies them out); no ownership transfer.
        r.samples.withUnsafeBufferPointer { cb?(ctx, $0.baseAddress, $0.count, Int32(r.sampleRate)) }
        return 0
    case .failure(let e):
        logErr("smk_synthesize error: \(e)")
        return 1
    }
}

@_cdecl("smk_shutdown")
public func smk_shutdown() {
    state.lock.lock()
    let mgr = state.manager
    state.manager = nil
    state.lock.unlock()
    if let mgr {
        _ = runBlocking({
            await mgr.cleanup()
            return true
        })
    }
}

// MARK: - ASR (Parakeet TDT v2, English, Core ML / ANE) — the apple-native STT backend

private final class AsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: AsrManager?
}
private let asr = AsrState()

/// Download (first use) + load the Parakeet TDT v2 (English-only) Core ML models and
/// build the ASR manager. English-only by design — mirrors the ONNX STT path, which
/// uses the v2 model too; v3 (25-language multilingual) is deliberately NOT used.
/// `model_dir` "" → FluidAudio's default cache. Returns 0 on success.
@_cdecl("smk_asr_init")
public func smk_asr_init(_ modelDir: UnsafePointer<CChar>?, _ computeUnits: Int32) -> Int32 {
    asr.lock.lock()
    defer { asr.lock.unlock() }
    DownloadUtils.enforceOffline = true  // load-only: DontSpeak pre-downloads the Parakeet set
    let dir = cString(modelDir).map { URL(fileURLWithPath: $0) }
    switch runBlocking({ () -> AsrManager in
        // `load(from:)` (not `downloadAndLoad`) reads the already-present models — it resolves
        // `<parent-of-dir>/parakeet-tdt-0.6b-v2`, exactly where our downloader placed them.
        guard let dir else { throw SmkError.nilDir }
        let models = try await AsrModels.load(from: dir, version: .v2)
        let mgr = AsrManager(config: .default)
        try await mgr.loadModels(models)
        return mgr
    }) {
    case .success(let mgr):
        asr.manager = mgr
        return 0
    case .failure(let e):
        logErr("smk_asr_init error: \(e)")
        return 1
    }
}

/// Transcribe 16 kHz mono f32 PCM → UTF-8 text. Caller owns *out_text; free via
/// smk_free_str. Empty input yields an empty string (rc 0).
@_cdecl("smk_transcribe")
public func smk_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    asr.lock.lock()
    let mgr = asr.manager
    asr.lock.unlock()
    guard let mgr else {
        logErr("smk_transcribe: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    switch runBlocking({ () -> String in
        // Parakeet TDT is stateless per utterance — a fresh decoder state each call.
        var decoderState = TdtDecoderState.make(decoderLayers: await mgr.decoderLayerCount)
        let result = try await mgr.transcribe(audio, decoderState: &decoderState, language: nil)
        return result.text
    }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        logErr("smk_transcribe error: \(e)")
        return 1
    }
}

@_cdecl("smk_asr_shutdown")
public func smk_asr_shutdown() {
    asr.lock.lock()
    let mgr = asr.manager
    asr.manager = nil
    asr.lock.unlock()
    if let mgr {
        _ = runBlocking({
            await mgr.cleanup()
            return true
        })
    }
}

// MARK: - Streaming ASR (FluidAudio StreamingEouAsrManager, Core ML / ANE)
//
// The cache-aware STREAMING counterpart of `smk_transcribe`: feed 16 kHz chunks as they arrive
// (encoder cache threaded inside FluidAudio — each frame encoded once), instead of re-transcribing
// the whole buffer per preview. Drives the SAME helper loop as the ONNX streaming path via the
// Rust `CoremlStreamer` (start/push/finish == reset/accept/finalize).
//
// NOTE: `process(audioBuffer:)` deliberately returns "" mid-stream (it decodes incrementally but
// only surfaces text from `finish()` / the EOU callback). So `smk_asr_stream_push` reads the
// running hypothesis via `getPartialTranscript()` after each chunk to feed the live overlay — see
// the call site below.
private final class StreamAsrState: @unchecked Sendable {
    let lock = NSLock()
    var manager: StreamingEouAsrManager?
}
private let streamAsr = StreamAsrState()

/// Begin a new streaming utterance: build/load the streaming manager on first use (from
/// `modelDir`, the streaming EOU Core ML model dir DontSpeak pre-downloaded), then reset its
/// per-utterance state. Returns 0 on success. `modelDir` is only consulted on the first call.
@_cdecl("smk_asr_stream_start")
public func smk_asr_stream_start(_ modelDir: UnsafePointer<CChar>?) -> Int32 {
    streamAsr.lock.lock()
    defer { streamAsr.lock.unlock() }
    DownloadUtils.enforceOffline = true  // DontSpeak pre-downloads the streaming model set
    let dir = cString(modelDir).map { URL(fileURLWithPath: $0) }
    switch runBlocking({ () -> StreamingEouAsrManager in
        if let mgr = streamAsr.manager {
            await mgr.reset()
            return mgr
        }
        guard let dir else { throw SmkError.nilDir }
        let mgr = StreamingEouAsrManager(chunkSize: .ms160)  // lowest latency (~6 partials/sec)
        try await mgr.loadModels(from: dir)
        await mgr.reset()
        return mgr
    }) {
    case .success(let mgr):
        streamAsr.manager = mgr
        return 0
    case .failure(let e):
        logErr("smk_asr_stream_start error: \(e)")
        return 1
    }
}

/// Feed a 16 kHz mono chunk; hand back the running hypothesis-so-far (via `getPartialTranscript`,
/// since `process` itself returns "" mid-stream). Caller frees *out via smk_free_str.
@_cdecl("smk_asr_stream_push")
public func smk_asr_stream_push(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    streamAsr.lock.lock()
    let mgr = streamAsr.manager
    streamAsr.lock.unlock()
    guard let mgr else {
        logErr("smk_asr_stream_push: not started")
        return 2
    }
    // StreamingEouAsrManager.process expects an AVAudioPCMBuffer and resamples it to 16 kHz
    // mono Float32 internally. Copy the caller's chunk into a Sendable [Float] and build the
    // (non-Sendable) buffer INSIDE the closure — capturing the buffer here would violate the
    // @Sendable contract of runBlocking.
    let audio = samples.map { Array(UnsafeBufferPointer(start: $0, count: n)) } ?? []
    let rate = Double(sampleRate)
    switch runBlocking({ () -> String in
        guard rate > 0,
            let format = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: rate, channels: 1, interleaved: false),
            let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                frameCapacity: AVAudioFrameCount(max(audio.count, 1)))
        else { throw SmkError.badAudio }
        buffer.frameLength = AVAudioFrameCount(audio.count)
        if !audio.isEmpty, let dst = buffer.floatChannelData {
            audio.withUnsafeBufferPointer { dst[0].update(from: $0.baseAddress!, count: audio.count) }
        }
        // `process()` decodes the chunk but RETURNS "" by design (it only yields text from
        // `finish()` / the EOU callback) — so reading its result gave the overlay nothing mid-stream.
        // Pull the running hypothesis explicitly: `getPartialTranscript()` decodes the accumulated
        // token ids, i.e. the same transcript-so-far `finish()` will return, growing per chunk.
        _ = try await mgr.process(audioBuffer: buffer)
        return await mgr.getPartialTranscript()
    }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        logErr("smk_asr_stream_push error: \(e)")
        return 1
    }
}

/// Flush the stream and return the final transcript. Caller frees *out via smk_free_str.
@_cdecl("smk_asr_stream_finish")
public func smk_asr_stream_finish(
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    streamAsr.lock.lock()
    let mgr = streamAsr.manager
    streamAsr.lock.unlock()
    guard let mgr else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    switch runBlocking({ () -> String in try await mgr.finish() }) {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        logErr("smk_asr_stream_finish error: \(e)")
        return 1
    }
}

// MARK: - System STT (Apple on-device recognition, en-US)
//
// The `system` STT engine, in two OS-version tiers:
//   • macOS 26+ — the modern SpeechAnalyzer / SpeechTranscriber API: async/await-native
//     (NO run loop, works from the helper's run-loop-less worker thread), better models,
//     no Speech-Recognition authorization needed. Per-locale model downloads on enable.
//   • macOS 14–25 — legacy `SFSpeechRecognizer` with `requiresOnDeviceRecognition`. Its
//     result handlers default to the app's MAIN queue, which deadlocks here (no main run
//     loop) — that bug once killed this path; the fix is `recognizer.queue = <private
//     OperationQueue>`, after which the path is plain synchronous code parking the calling
//     C thread on a semaphore (no Swift concurrency needed). Unlike 26+, this tier DOES
//     need Speech-Recognition authorization (TCC prompt; NSSpeechRecognitionUsageDescription
//     is in the app's Info.plist).
// Both tiers are on-device only — audio never leaves the machine, no server fallback.
//
// Status codes (smk_sys_available / smk_sys_authorize):
//   0 = ready, 1 = preparing (26+: model not installed yet; <26: permission not requested
//   yet — the authorize gate / first dictation prompts), 2 = no on-device recognition for
//   the locale, 3 = macOS too old (below the app floor; unreachable in practice),
//   4 = Speech-Recognition permission denied (<26 only; 26+ needs no authorization).

private let SYS_LOCALE = Locale(identifier: "en-US")

/// Mono f32 PCM → AVAudioPCMBuffer at `sampleRate`. Shared by both System STT tiers.
private func sysMakeBuffer(_ samples: [Float], sampleRate: Double) -> AVAudioPCMBuffer? {
    guard
        let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: sampleRate, channels: 1, interleaved: false),
        let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count))
    else { return nil }
    buf.frameLength = AVAudioFrameCount(samples.count)
    samples.withUnsafeBufferPointer { src in
        if let dst = buf.floatChannelData?[0], let base = src.baseAddress {
            dst.update(from: base, count: samples.count)
        }
    }
    return buf
}

@available(macOS 26, *)
private func sysLocaleSupported() async -> Bool {
    await SpeechTranscriber.supportedLocales
        .contains { $0.identifier(.bcp47) == SYS_LOCALE.identifier(.bcp47) }
}

@available(macOS 26, *)
private func sysModelInstalled() async -> Bool {
    await SpeechTranscriber.installedLocales
        .contains { $0.identifier(.bcp47) == SYS_LOCALE.identifier(.bcp47) }
}

@available(macOS 26, *)
private func sysEnsureModel(_ transcriber: SpeechTranscriber) async throws {
    guard !(await sysModelInstalled()) else { return }
    if let req = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
        try await req.downloadAndInstall()
    }
}

// MARK: legacy tier (macOS 14–25, SFSpeechRecognizer)

/// Where legacy recognition callbacks land — NEVER the main queue (see the section
/// comment: an undelivered main-queue callback is the historical deadlock).
private let legacyQueue: OperationQueue = {
    let q = OperationQueue()
    q.name = "smkokoro.sysstt.legacy"
    q.maxConcurrentOperationCount = 1
    return q
}()

/// en-US recognizer with off-main callback delivery, or nil when the locale has no
/// recognizer at all.
private func legacyRecognizer() -> SFSpeechRecognizer? {
    guard let r = SFSpeechRecognizer(locale: SYS_LOCALE) else { return nil }
    r.queue = legacyQueue
    return r
}

/// Non-prompting availability for the legacy tier. "Preparing" here means the permission
/// hasn't been requested yet (there is no separate model download step — the OS recognizer
/// is resident once on-device support exists).
private func legacyAvailable() -> Int32 {
    guard let r = legacyRecognizer(), r.supportsOnDeviceRecognition else { return 2 }
    switch SFSpeechRecognizer.authorizationStatus() {
    case .authorized: return 0
    case .notDetermined: return 1
    default: return 4
    }
}

/// ENABLE the legacy tier: request Speech-Recognition authorization (the one-time TCC
/// prompt), BLOCKING. `requestAuthorization` is a class method whose completion queue is
/// undocumented (possibly the main queue, which never drains here) — so poll the TCC state
/// instead of trusting the callback, and give the user up to 120 s to answer the dialog.
private func legacyAuthorize() -> Int32 {
    guard let r = legacyRecognizer(), r.supportsOnDeviceRecognition else { return 2 }
    if SFSpeechRecognizer.authorizationStatus() == .notDetermined {
        let sem = DispatchSemaphore(value: 0)
        SFSpeechRecognizer.requestAuthorization { _ in sem.signal() }
        let deadline = DispatchTime.now() + .seconds(120)
        while SFSpeechRecognizer.authorizationStatus() == .notDetermined,
            DispatchTime.now() < deadline,
            sem.wait(timeout: .now() + .milliseconds(250)) == .timedOut
        {}
    }
    // notDetermined after the wait = prompt unanswered/undelivered — report as denied
    // (rc 4's remedy text covers it); a later authorize can still succeed.
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else { return 4 }
    // `supportsOnDeviceRecognition` lies when macOS Dictation is switched off: the local
    // recognition service then refuses every request (kLSRErrorDomain 201, "Siri and
    // Dictation are disabled"). Smoke-run a beat of silence so enabling FAILS here with a
    // clear remedy instead of shipping a green dot that can't transcribe.
    switch legacyTranscribe([Float](repeating: 0, count: 3_200), sampleRate: 16_000) {
    case .success:
        return 0
    case .failure(let e):
        let ns = e as NSError
        logErr("system STT smoke recognition failed: \(e)")
        return ns.domain == "kLSRErrorDomain" && ns.code == 201 ? 2 : 0
    }
}

/// One batch recognition's shared state: the callback (on `legacyQueue`) fills it, the
/// calling C thread parks on `sem`. Also RETAINS the recognizer + task for the duration —
/// nothing else keeps them alive. `finish` is single-shot: a trailing error after the
/// final result (or a post-timeout callback) must not clobber the outcome.
private final class LegacyRun: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    var text: String?
    var error: Error?
    var recognizer: SFSpeechRecognizer?
    var task: SFSpeechRecognitionTask?
    let sem = DispatchSemaphore(value: 0)
    func finish(text: String?, error: Error?) {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return }
        done = true
        self.text = text
        self.error = error
        sem.signal()
    }
}

/// kAFAssistantErrorDomain 1110 — "no speech detected". A silent segment is a normal
/// dictation outcome, not a failure: map it to the empty transcript.
private func legacyIsNoSpeech(_ error: Error) -> Bool {
    let e = error as NSError
    return e.domain == "kAFAssistantErrorDomain" && e.code == 1110
}

/// Batch-transcribe on the legacy tier, synchronously on the calling thread. Prompts for
/// authorization on demand (mirrors the 26+ tier's download-on-first-dictation), then runs
/// one on-device `SFSpeechAudioBufferRecognitionRequest` with a bounded wait so a wedged
/// recognizer can't hang the helper forever.
private func legacyTranscribe(_ samples: [Float], sampleRate: Double) -> Result<String, Error> {
    if SFSpeechRecognizer.authorizationStatus() == .notDetermined {
        _ = legacyAuthorize()
    }
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else {
        return .failure(SmkError.sysUnavailable("speech recognition permission not granted"))
    }
    guard let recognizer = legacyRecognizer(), recognizer.supportsOnDeviceRecognition else {
        return .failure(SmkError.sysUnavailable("no on-device recognition for en-US"))
    }
    guard sampleRate > 0, let buffer = sysMakeBuffer(samples, sampleRate: sampleRate) else {
        return .failure(SmkError.badAudio)
    }

    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = false

    let run = LegacyRun()
    run.recognizer = recognizer
    run.task = recognizer.recognitionTask(with: request) { result, error in
        if let result, result.isFinal {
            run.finish(text: result.bestTranscription.formattedString, error: nil)
        } else if let error {
            run.finish(text: legacyIsNoSpeech(error) ? "" : nil, error: legacyIsNoSpeech(error) ? nil : error)
        }
    }
    request.append(buffer)
    request.endAudio()

    // Recognition of a batch is faster than real time; audio length + 30 s is generous.
    let margin = Int(Double(samples.count) / sampleRate) + 30
    if run.sem.wait(timeout: .now() + .seconds(margin)) == .timedOut {
        run.task?.cancel()
        run.finish(text: nil, error: SmkError.sysUnavailable("recognition timed out"))
    }
    if let text = run.text { return .success(text) }
    return .failure(run.error ?? SmkError.noResult)
}

// MARK: C entry points (both tiers)

/// Current usability WITHOUT prompting/downloading (safe for the frequent model-status
/// poll). Status codes as per the section comment; the engine maps 1 → the orange
/// "preparing" dot (mirrors Parakeet warming).
@_cdecl("smk_sys_available")
public func smk_sys_available() -> Int32 {
    guard #available(macOS 26, *) else { return legacyAvailable() }
    switch runBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 2 }
        return await sysModelInstalled() ? 0 : 1
    }) {
    case .success(let code): return code
    default: return 2
    }
}

/// ENABLE the engine, BLOCKING. 26+: download the en-US on-device model if needed (the
/// one-time first-use cost; SpeechAnalyzer needs no Speech-Recognition authorization —
/// the model is the only gate). <26: request Speech-Recognition authorization (the
/// one-time TCC prompt). 0 when ready; other codes per the section comment.
@_cdecl("smk_sys_authorize")
public func smk_sys_authorize() -> Int32 {
    guard #available(macOS 26, *) else { return legacyAuthorize() }
    switch runBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 2 }
        let transcriber = SpeechTranscriber(locale: SYS_LOCALE, preset: .transcription)
        try await sysEnsureModel(transcriber)
        return 0
    }) {
    case .success(let code): return code
    case .failure(let e):
        logErr("smk_sys_authorize error: \(e)")
        return 2
    }
}

/// Transcribe 16 kHz mono f32 PCM → UTF-8 text on-device, as ONE batch; `cb` borrows the
/// text (copied out during the call). Empty input → empty string. rc: 0 ok, 1 error.
@_cdecl("smk_sys_transcribe")
public func smk_sys_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    guard let samples, n > 0 else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    let pcm = Array(UnsafeBufferPointer(start: samples, count: n))
    let outcome: Result<String, Error>
    if #available(macOS 26, *) {
        outcome = runBlocking({ try await sysTranscribe(pcm, sampleRate: Double(sampleRate)) })
    } else {
        outcome = legacyTranscribe(pcm, sampleRate: Double(sampleRate))
    }
    switch outcome {
    case .success(let text):
        text.withCString { cb?(ctx, $0) }
        return 0
    case .failure(let e):
        logErr("smk_sys_transcribe error: \(e)")
        return 1
    }
}

/// Run one batch transcription through SpeechAnalyzer + SpeechTranscriber (on-device).
@available(macOS 26, *)
private func sysTranscribe(_ samples: [Float], sampleRate: Double) async throws -> String {
    let transcriber = SpeechTranscriber(locale: SYS_LOCALE, preset: .transcription)
    try await sysEnsureModel(transcriber)
    let analyzer = SpeechAnalyzer(modules: [transcriber])

    // Wrap our mono f32 PCM at its real rate, then convert to the analyzer's preferred
    // format if it differs.
    guard let inBuf = sysMakeBuffer(samples, sampleRate: sampleRate) else { return "" }
    var buffer = inBuf
    if let target = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]),
        target != inBuf.format,
        let converted = try sysConvert(inBuf, to: target)
    {
        buffer = converted
    }

    // Feed the single buffer, finish input, finalize, then drain the results.
    let (stream, cont) = AsyncStream<AnalyzerInput>.makeStream()
    try await analyzer.start(inputSequence: stream)
    cont.yield(AnalyzerInput(buffer: buffer))
    cont.finish()
    try await analyzer.finalizeAndFinishThroughEndOfInput()

    var text = ""
    for try await result in transcriber.results where result.isFinal {
        text += String(result.text.characters)
    }
    return text
}

/// One-shot input source for AVAudioConverter: hands the whole buffer over on the first
/// pull, then signals end-of-stream. A reference holder so the converter's @Sendable input
/// block captures no mutable var / non-Sendable buffer directly.
private final class ConvertFeed: @unchecked Sendable {
    let buffer: AVAudioPCMBuffer
    var done = false
    init(_ b: AVAudioPCMBuffer) { buffer = b }
}

/// Convert a PCM buffer to `format` (sample-rate + layout) via AVAudioConverter.
@available(macOS 26, *)
private func sysConvert(_ input: AVAudioPCMBuffer, to format: AVAudioFormat) throws -> AVAudioPCMBuffer? {
    guard let converter = AVAudioConverter(from: input.format, to: format) else { return nil }
    let ratio = format.sampleRate / input.format.sampleRate
    let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1024
    guard let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return nil }
    let feed = ConvertFeed(input)
    var error: NSError?
    converter.convert(to: output, error: &error) { _, status in
        if feed.done {
            status.pointee = .endOfStream
            return nil
        }
        feed.done = true
        status.pointee = .haveData
        return feed.buffer
    }
    if let error { throw error }
    return output
}

// MARK: - Diarization (Pyannote segmentation + WeSpeaker embeddings, Core ML / ANE)
//
// "Who spoke when" — the apple-native speaker-diarization backend. A third subsystem
// of this shim (smk_diar_*), mirroring the ASR one: dlopen'd from the SAME dylib, its
// own FluidAudio manager + lock. Models (Pyannote + WeSpeaker) auto-download on first
// init. Output is JSON so the C ABI stays one string wide; the Rust side parses it.

private final class DiarState: @unchecked Sendable {
    let lock = NSLock()
    var manager: DiarizerManager?
}
private let diar = DiarState()

/// Download (first use) + load the Pyannote segmentation + WeSpeaker embedding Core ML
/// models and build the diarizer. `model_dir` "" → FluidAudio's default cache. Returns 0
/// on success. `clustering_threshold` tunes how readily distinct embeddings split into
/// separate speakers (FluidAudio range 0.5–0.9, lower = MORE speakers); pass <= 0 to use
/// FluidAudio's default (0.7).
@_cdecl("smk_diar_init")
public func smk_diar_init(_ modelDir: UnsafePointer<CChar>?, _ clusteringThreshold: Float) -> Int32 {
    diar.lock.lock()
    defer { diar.lock.unlock() }
    // debugMode makes performCompleteDiarization populate `speakerDatabase` (per-speaker
    // embeddings), which we surface so the engine can match clusters to enrolled voiceprints.
    // Built as a `let` (immutable) so it's safe to capture in the @Sendable runBlocking closure.
    let config: DiarizerConfig = {
        var c =
            clusteringThreshold > 0
            ? DiarizerConfig(clusteringThreshold: clusteringThreshold)
            : DiarizerConfig()
        c.debugMode = true
        return c
    }()
    // DontSpeak pre-downloads the two diarization models into `<model_dir>/speaker-diarization-
    // coreml`; load them DIRECTLY from there (no network) via FluidAudio's local-file API.
    // CONTRACT: the folder + the two `.mlmodelc` basenames below MIRROR the Rust consts
    // `DIARIZATION_COREML_DIR_NAME` / `DIARIZATION_SEGMENTATION_MODEL` / `DIARIZATION_EMBEDDING_MODEL`
    // in `ds-model/src/coreml_repo.rs` (which is where they're downloaded). Keep them
    // byte-identical — a mismatch makes this offline load fail with `modelMissing`. The Rust
    // `diarization_model_names_match_prefixes` test pins the Rust half.
    DownloadUtils.enforceOffline = true
    let dir = cString(modelDir).map { URL(fileURLWithPath: $0) }
    switch runBlocking({ () -> DiarizerManager in
        guard let dir else { throw SmkError.nilDir }
        let base = dir.appendingPathComponent("speaker-diarization-coreml")
        let models = try DiarizerModels.load(
            localSegmentationModel: base.appendingPathComponent("pyannote_segmentation.mlmodelc"),
            localEmbeddingModel: base.appendingPathComponent("wespeaker_v2.mlmodelc")
        )
        let mgr = DiarizerManager(config: config)
        mgr.initialize(models: models)
        return mgr
    }) {
    case .success(let mgr):
        diar.manager = mgr
        return 0
    case .failure(let e):
        logErr("smk_diar_init error: \(e)")
        return 1
    }
}

/// Diarize 16 kHz mono f32 PCM → UTF-8 JSON:
///   {"segments":[{"speaker":"<id>","start":0.0,"end":2.34}, ...],
///    "speakers":{"<id>":[..floats..]}}
/// Each segment's `speaker` and the `speakers` map share ONE id-space (FluidAudio's
/// speakerId) so the engine can join them to relabel clusters by enrolled name.
/// Caller owns *out_json; free via smk_free_str. Empty input yields {"segments":[]} (rc 0).
@_cdecl("smk_diarize")
public func smk_diarize(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    _ = sampleRate  // FluidAudio expects 16 kHz mono; the caller resamples upstream.
    // Hold the lock for the FULL call (not just the manager read): DiarizerManager is a
    // plain class with mutable Sendable-struct state (speakerManager), not an actor like
    // the TTS/ASR managers, so concurrent calls into it race unless fully serialized here.
    diar.lock.lock()
    defer { diar.lock.unlock() }
    guard let mgr = diar.manager else {
        logErr("smk_diarize: not initialized")
        return 2
    }
    guard let samples, n > 0 else {
        "{\"segments\":[]}".withCString { cb?(ctx, $0) }
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
        // Per-speaker embeddings (debugMode) so the engine can match clusters to enrolled
        // voiceprints. CONTRACT: this map is keyed by the SAME id string that appears as
        // each segment's `speaker` (seg.speakerId) — the engine joins speakers→segments
        // on that single id-space. We build it by walking the segments' ids and pulling
        // each one's voiceprint from speakerDatabase, so every key here is guaranteed to
        // occur in `segments` (no orphan ids from a divergent db key-space). Absent db /
        // unmatched id → an empty/partial map; the engine then keeps the numeric id.
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
        String(decoding: data, as: UTF8.self).withCString { cb?(ctx, $0) }
        return 0
    } catch {
        logErr("smk_diarize error: \(error)")
        return 1
    }
}

@_cdecl("smk_diar_shutdown")
public func smk_diar_shutdown() {
    diar.lock.lock()
    diar.manager = nil
    diar.lock.unlock()
}

/// Extract a single WeSpeaker voiceprint embedding from 16 kHz mono f32 PCM — the
/// enrollment primitive. Requires the diarizer to be initialized (`smk_diar_init`).
/// Caller owns *out_floats; free via `smk_free`. Empty input → rc 3.
@_cdecl("smk_diar_embed")
public func smk_diar_embed(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkPcmCb?
) -> Int32 {
    _ = sampleRate  // FluidAudio expects 16 kHz mono; the caller resamples upstream.
    // See smk_diarize: hold the lock for the full call to serialize access to the
    // non-actor DiarizerManager's mutable state.
    diar.lock.lock()
    defer { diar.lock.unlock() }
    guard let mgr = diar.manager else {
        logErr("smk_diar_embed: not initialized")
        return 2
    }
    guard let samples, n > 0 else { return 3 }
    let audio = Array(UnsafeBufferPointer(start: samples, count: n))
    do {
        let emb = try mgr.extractSpeakerEmbedding(from: audio)
        // Borrow the embedding to the callback (sample_rate is irrelevant for an embedding).
        emb.withUnsafeBufferPointer { cb?(ctx, $0.baseAddress, $0.count, 0) }
        return 0
    } catch {
        logErr("smk_diar_embed error: \(error)")
        return 1
    }
}

/// Download (if absent) just the diarization models — an explicit pre-download path
/// (vs. lazy download on first init). Returns 0 on success (or already-present). Does
/// NOT build a manager.
@_cdecl("smk_diar_download")
public func smk_diar_download() -> Int32 {
    switch runBlocking({ () -> Bool in
        _ = try await DiarizerModels.downloadIfNeeded()
        return true
    }) {
    case .success:
        return 0
    case .failure(let e):
        logErr("smk_diar_download error: \(e)")
        return 1
    }
}
