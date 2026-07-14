// libsmkokoro — a thin C ABI over FluidAudio's ANE Kokoro, dlopen'd by the
// DontSpeak Rust helper. Kokoro-compatible IPA phonemes in, 24 kHz mono fp32 PCM out.
// Rust owns the single text frontend used by both ONNX and Core ML; this shim bypasses
// FluidAudio's separate G2P so pronunciation and chunk boundaries cannot drift by backend.
//
// C ABI (see smkokoro.h):
//   int32 smk_init(const char* model_dir, int32 compute_units)
//   int32 smk_synthesize_phonemes(const char* phonemes, const char* voice, float speed,
//                                  void* ctx, smk_pcm_cb cb)
//   void  smk_shutdown(void)
//
// Returns 0 on success, non-zero on error. The helper drives synthesis serially;
// a lock guards the shared manager for safety.
import AVFoundation
import FluidAudio
import Foundation
import os
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

@_cdecl("smk_synthesize_phonemes")
public func smk_synthesize_phonemes(
    _ phonemes: UnsafePointer<CChar>?,
    _ voice: UnsafePointer<CChar>?,
    _ speed: Float,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkPcmCb?
) -> Int32 {
    guard let cb else { return 4 }
    state.lock.lock()
    let mgr = state.manager
    state.lock.unlock()
    guard let mgr else {
        logErr("smk_synthesize: not initialized")
        return 2
    }
    guard let p = cString(phonemes) else { return 3 }
    let v = cString(voice)
    switch runBlocking({
        try await mgr.synthesizeFromPhonemesDetailed(p, voice: v, speed: speed)
    }) {
    case .success(let r):
        // Borrow the samples to the callback (it copies them out); no ownership transfer.
        r.samples.withUnsafeBufferPointer { cb(ctx, $0.baseAddress, $0.count, Int32(r.sampleRate)) }
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

/// #34 data-collection sink for `legacySegmentDidReset`'s unretuned 0.65s phrase-gap
/// constant — see `LegacyRun.recordPartial`. `.debug`-level only: invisible/unpersisted
/// unless a developer explicitly enables debug capture for this subsystem (mirrors the
/// Rust engine's own DONTSPEAK_DEBUG-gated verbose telemetry — see ds-log's
/// `LogLevel::Debug` doc). Never logs dictated text, only the measured gap.
private let legacyResetLog = Logger(subsystem: "app.dontspeak.org", category: "legacy-stt")

/// One batch recognition's shared state: the callback (on `legacyQueue`) fills it, the
/// calling C thread parks on `sem`. Also RETAINS the recognizer + task for the duration —
/// nothing else keeps them alive. `finish` is single-shot: a trailing error after the
/// final result (or a post-timeout callback) must not clobber the outcome.
///
/// `committedText`/`latestPartial` are used ONLY by the streaming entry points
/// (`sysStreamStartLegacy` and friends) — `legacyTranscribe`'s one-shot batch use never
/// touches them, so they just sit at their empty default there. They're owned by THIS run
/// object rather than `SysStreamState` (finding #2 follow-up) so a stale/replaced run's own
/// callback can only ever read/write ITS OWN copy — no cross-run corruption is possible by
/// construction, and `smk_sys_stream_finish`'s own `isFinal` callback can still read them
/// correctly even after `SysStreamState`'s shared `legacyRun`/`legacyRequest` pointers have
/// already been cleared (which happens as soon as `finish` starts waiting — a check against
/// those shared pointers at that point would incorrectly read this, the CORRECT run, as
/// stale).
private final class LegacyRun: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    var text: String?
    var error: Error?
    var recognizer: SFSpeechRecognizer?
    var task: SFSpeechRecognitionTask?
    let sem = DispatchSemaphore(value: 0)

    private let textLock = NSLock()
    private var committedText: String = ""
    private var latestPartial: String = ""
    private var lastPartialAt: TimeInterval?

    func finish(text: String?, error: Error?) {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return }
        done = true
        self.text = text
        self.error = error
        sem.signal()
    }

    /// `committedText + " " + latestPartial` (whichever half is non-empty) — same shape as
    /// `SysModernSession.hypothesis()`.
    func hypothesis() -> String {
        textLock.lock()
        defer { textLock.unlock() }
        return legacyJoin(committedText, latestPartial)
    }

    /// Record one non-final result's `bestTranscription`. Detects a phrase-segment boundary
    /// (`legacySegmentDidReset` — see its doc comment) and, if one just happened, commits the
    /// pre-reset `latestPartial` into `committedText` (finding #7's pattern: skip an empty
    /// segment so it can't bake in a stray separator) before starting the new segment.
    func recordPartial(_ newText: String) {
        textLock.lock()
        defer { textLock.unlock() }
        let now = ProcessInfo.processInfo.systemUptime
        let gap = lastPartialAt.map { now - $0 }
        if legacySegmentDidReset(previous: latestPartial, new: newText, gapSeconds: gap), !latestPartial.isEmpty {
            // #34: the 0.65s phrase-gap constant in `legacySegmentDidReset` is asserted, not
            // measured (unlike the paired <0.5 shared-prefix-ratio threshold, which has an
            // empirical basis — see that function's doc). This records the ACTUAL gap that fired
            // a reset on a real System STT session, so a future retune has real data instead of
            // none. `.public` privacy is deliberate: this is a numeric/boolean measurement, never
            // the dictated text itself. Interpolates the raw `TimeInterval` via os.Logger's own
            // lazy `format:` specifier rather than pre-formatting with `String(format:)` — the
            // whole point of os.Logger is that formatting is deferred until the log point is
            // actually collected, which a pre-built String would defeat on every phrase reset.
            if let gap {
                legacyResetLog.debug(
                    "legacy STT phrase reset: gapSeconds=\(gap, format: .fixed(precision: 3), privacy: .public)")
            } else {
                legacyResetLog.debug("legacy STT phrase reset: gapSeconds=nil")
            }
            committedText = legacyJoin(committedText, latestPartial)
        }
        latestPartial = newText
        lastPartialAt = now
    }

    /// `committedText + " " + finalSegment` — the shape `isFinal`'s callback hands to
    /// `finish`, joining whatever was already committed with the last (final) segment.
    func finalJoined(_ finalSegment: String) -> String {
        textLock.lock()
        defer { textLock.unlock() }
        return legacyJoin(committedText, finalSegment)
    }
}

/// kAFAssistantErrorDomain 1110 — "no speech detected". A silent segment is a normal
/// dictation outcome, not a failure: map it to the empty transcript.
private func legacyIsNoSpeech(_ error: Error) -> Bool {
    let e = error as NSError
    return e.domain == "kAFAssistantErrorDomain" && e.code == 1110
}

/// Shared auth+recognizer gate for BOTH legacy entry points (batch `legacyTranscribe` and
/// streaming `sysStreamStartLegacy`): prompt for authorization on demand (mirrors the 26+
/// tier's download-on-first-dictation), then hand back a ready-to-use recognizer. Factored out
/// (finding #6) — this exact sequence used to be copy-pasted in both places.
private func legacyEnsureAuthorizedRecognizer() -> Result<SFSpeechRecognizer, Error> {
    if SFSpeechRecognizer.authorizationStatus() == .notDetermined {
        _ = legacyAuthorize()
    }
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else {
        return .failure(SmkError.sysUnavailable("speech recognition permission not granted"))
    }
    guard let recognizer = legacyRecognizer(), recognizer.supportsOnDeviceRecognition else {
        return .failure(SmkError.sysUnavailable("no on-device recognition for en-US"))
    }
    return .success(recognizer)
}

/// Batch-transcribe on the legacy tier, synchronously on the calling thread. Prompts for
/// authorization on demand (mirrors the 26+ tier's download-on-first-dictation), then runs
/// one on-device `SFSpeechAudioBufferRecognitionRequest` with a bounded wait so a wedged
/// recognizer can't hang the helper forever.
private func legacyTranscribe(_ samples: [Float], sampleRate: Double) -> Result<String, Error> {
    let recognizer: SFSpeechRecognizer
    switch legacyEnsureAuthorizedRecognizer() {
    case .success(let r): recognizer = r
    case .failure(let e): return .failure(e)
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

// MARK: - System STT streaming (incremental hypothesis, mirrors smk_asr_stream_*)
//
// Gives the `system` engine real incremental streaming instead of the periodic full-tail
// re-transcription the Rust `transcribe_loop` fallback still does for it. Same three-call
// shape as `smk_asr_stream_*` above (start/push/finish == reset/accept/finalize on the Rust
// `StreamingStt` side), but driving Apple's own recognizer per tier:
//   • macOS 26+: ONE persistent `SpeechAnalyzer` + `AsyncStream<AnalyzerInput>` for the whole
//     utterance. A detached `Task` drains `transcriber.results` WITHOUT the `where
//     result.isFinal` filter `sysTranscribe` uses, so we see volatile (non-final) results too:
//     `committedText` accumulates each finalized result, `latestVolatileText` holds the newest
//     in-flight (non-final) hypothesis. It runs independently of whichever thread is currently
//     inside a push/finish call — those only read the two fields under the session's lock.
//   • Legacy (<26): a FRESH `SFSpeechAudioBufferRecognitionRequest` per utterance (like
//     `legacyTranscribe`), but with `shouldReportPartialResults = true` — today's batch path
//     sets it `false`, which is the actual bug this streaming path fixes.
//
//     `SFSpeechRecognizer` on this tier gives NO explicit signal for a phrase-segment
//     boundary: empirically (real `say`-generated speech, real-time-paced across a 3.5s
//     pause, every callback logged), after a pause the task stays `.running` and `isFinal`
//     never fires early — `bestTranscription` just silently resets to a fresh short
//     hypothesis on the very next non-final callback, confirmed across MULTIPLE pauses in
//     the same request/task without it ever dying. So the ONE task/request lives for the
//     whole utterance (no restart needed), but its `bestTranscription` only ever covers the
//     CURRENT phrase segment — mirroring `SysModernSession`, `LegacyRun` (see its doc
//     comment for why it lives THERE and not on `SysStreamState`) accumulates each
//     completed segment in `committedText` and tracks the CURRENT segment's in-flight
//     hypothesis in `latestPartial` (not the whole utterance's, as before). A segment
//     boundary is inferred from the text itself in `legacySegmentDidReset` (see its doc
//     comment for the exact heuristic and why a naive word/length check false-positives on
//     ordinary in-phrase revisions like digit re-grouping); when detected,
//     `LegacyRun.recordPartial` commits the pre-reset `latestPartial` into `committedText`
//     before overwriting it. `run.finish` only fires once, on `isFinal` (mirrors
//     `LegacyRun`'s guard-against-double-signal), and its text is
//     `LegacyRun.finalJoined(_:)` — `committedText + " " + <final segment>`, the same
//     committed+volatile shape `SysModernSession.hypothesis()` uses.
//
// Both tiers reuse the existing per-tier helpers (`sysEnsureModel`/`sysConvert`/`sysMakeBuffer`,
// `legacyRecognizer`/`legacyQueue`/`LegacyRun`/`legacyIsNoSpeech`) — no duplicated auth/model
// logic. `smk_sys_transcribe`/`legacyTranscribe`/`sysTranscribe` (batch) are untouched.

/// Modern-tier (26+) per-utterance session. Held as a plain (non-`@available`) `Any?` on
/// `SysStreamState` so the OUTER state class itself needs no availability gate; every site that
/// casts it back out is already inside a `#available(macOS 26, *)` check.
@available(macOS 26, *)
private final class SysModernSession: @unchecked Sendable {
    let lock = NSLock()
    let analyzer: SpeechAnalyzer
    let transcriber: SpeechTranscriber
    let continuation: AsyncStream<AnalyzerInput>.Continuation
    /// The analyzer's preferred audio format for `transcriber`, resolved ONCE at session start.
    /// It's invariant for the session's lifetime (depends only on the fixed transcriber), so
    /// `sysStreamConvert` reads this instead of re-deriving it via a blocking async round-trip
    /// on every push call.
    let targetFormat: AVAudioFormat?
    var drainTask: Task<Void, Never>?
    var committedText: String = ""
    var latestVolatileText: String = ""

    init(
        analyzer: SpeechAnalyzer, transcriber: SpeechTranscriber,
        continuation: AsyncStream<AnalyzerInput>.Continuation,
        targetFormat: AVAudioFormat?
    ) {
        self.analyzer = analyzer
        self.transcriber = transcriber
        self.continuation = continuation
        self.targetFormat = targetFormat
    }

    /// `committedText + " " + latestVolatileText` (whichever half is non-empty) — the running
    /// hypothesis `push`/`finish` hand back, same shape as `smk_asr_stream_push`'s
    /// `getPartialTranscript()`.
    func hypothesis() -> String {
        lock.lock()
        defer { lock.unlock() }
        if latestVolatileText.isEmpty { return committedText }
        if committedText.isEmpty { return latestVolatileText }
        return "\(committedText) \(latestVolatileText)"
    }

    /// Record one `transcriber.results` element. A plain (non-`async`) method so the drain
    /// task's `for try await` body can call it without tripping `NSLock.lock()`'s "unavailable
    /// from asynchronous contexts" restriction — the lock/unlock pair lives entirely inside this
    /// synchronous function, which is a legal (if blocking) call from any async context.
    func recordResult(_ text: String, isFinal: Bool) {
        lock.lock()
        defer { lock.unlock() }
        if isFinal {
            // An empty final (e.g. a silent segment) must NOT bake a permanent trailing
            // separator into committedText — only append when there's real text.
            if !text.isEmpty {
                committedText = committedText.isEmpty ? text : "\(committedText) \(text)"
            }
            latestVolatileText = ""
        } else {
            latestVolatileText = text
        }
    }
}

/// Publish the just-built modern-tier session. A plain (non-`async`) function for the same
/// reason as `SysModernSession.recordResult` — `sysStreamStartModern`'s `runBlocking` closure is
/// an async context, so the `sysStream.lock` lock/unlock pair has to live inside an ordinary
/// synchronous call instead of appearing inline there.
@available(macOS 26, *)
private func sysStreamSetModern(_ session: SysModernSession) {
    sysStream.lock.lock()
    defer { sysStream.lock.unlock() }
    sysStream.modern = session
}

/// Atomically pop whatever modern-tier session is currently installed (if any), clearing the
/// slot. Used by `sysStreamStartModern`'s self-heal (finding #1): a prior session must be torn
/// down before a new one replaces it, since `smk_sys_stream_start` used to overwrite the slot
/// unconditionally, leaking the old continuation/analyzer/drain-task whenever `finish` hadn't
/// run (e.g. a failed tail-flush skipped it — see `StreamSession::finalize` on the Rust side).
@available(macOS 26, *)
private func sysStreamTakeModern() -> SysModernSession? {
    sysStream.lock.lock()
    defer { sysStream.lock.unlock() }
    let prior = sysStream.modern as? SysModernSession
    sysStream.modern = nil
    return prior
}

/// Bound on tearing down a modern-tier session's analyzer — mirrors the legacy tier's explicit
/// 30s bound in `smk_sys_stream_finish` (finding #4): a wedged `SpeechAnalyzer` can't hang the
/// helper forever.
private let sysStreamModernFinishTimeoutSeconds: Double = 30

/// Resume a `CheckedContinuation` exactly once, no matter how many racing tasks try — extra
/// callers are silently dropped. `CheckedContinuation.resume` traps if called twice, and two
/// independent detached tasks (the real finish vs. the timeout) race to resume the same one in
/// `sysStreamTeardownModern` below, so this guard is required, not just tidy.
private final class SingleShotContinuation<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    func resumeOnce(_ continuation: CheckedContinuation<T, Never>, with value: T) {
        lock.lock()
        let already = done
        done = true
        lock.unlock()
        if !already { continuation.resume(returning: value) }
    }
}

/// Finish + drain a modern-tier session (finalize the analyzer, let the drain task consume the
/// last results, hand back the accumulated hypothesis), bounded by
/// `sysStreamModernFinishTimeoutSeconds` (finding #4) — mirrors the legacy tier's
/// semaphore-with-timeout in `smk_sys_stream_finish` below: race the real finish against a
/// timer and return whichever settles first, WITHOUT waiting on the loser. (A `TaskGroup`
/// can't express this — `withTaskGroup` implicitly awaits every child before returning, even
/// after `cancelAll()`, so it can't bound a call that ignores cancellation; a bare detached
/// task + a single-shot continuation can, at the cost of leaking the abandoned task if the
/// analyzer really is wedged — same trade the legacy tier makes with `run.task?.cancel()` not
/// being awaited either.) On timeout, returns whatever `session.hypothesis()` has accumulated
/// so far rather than blocking indefinitely. Shared by `smk_sys_stream_finish` (the normal
/// end-of-utterance path) and `sysStreamStartModern`'s self-heal teardown of a leaked prior
/// session (finding #1) — the return value is discarded there since the prior utterance is
/// being abandoned anyway.
@available(macOS 26, *)
private func sysStreamTeardownModern(_ session: SysModernSession) async -> String {
    session.continuation.finish()
    let gate = SingleShotContinuation<String>()
    return await withCheckedContinuation { (cont: CheckedContinuation<String, Never>) in
        Task.detached {
            do {
                try await session.analyzer.finalizeAndFinishThroughEndOfInput()
            } catch {
                logErr("sysStreamTeardownModern: finalize failed: \(error)")
            }
            _ = await session.drainTask?.value
            gate.resumeOnce(cont, with: session.hypothesis())
        }
        Task.detached {
            try? await Task.sleep(for: .seconds(sysStreamModernFinishTimeoutSeconds))
            logErr("sysStreamTeardownModern: timed out after \(sysStreamModernFinishTimeoutSeconds)s")
            gate.resumeOnce(cont, with: session.hypothesis())
        }
    }
}

private final class SysStreamState: @unchecked Sendable {
    let lock = NSLock()
    // Legacy tier (<26). The committed/current-segment split (finding #2 follow-up) lives
    // ON `legacyRun` itself now, not here — see `LegacyRun`'s doc comment for why.
    var legacyRun: LegacyRun?
    var legacyRequest: SFSpeechAudioBufferRecognitionRequest?
    // Modern tier (26+): type-erased `SysModernSession` (see its doc comment for why).
    var modern: Any?
}
private let sysStream = SysStreamState()

/// `committed + " " + current` (whichever half is non-empty) — the running hypothesis
/// shape shared by `LegacyRun.hypothesis()`/`finalJoined` and `SysModernSession.hypothesis()`.
private func legacyJoin(_ committed: String, _ current: String) -> String {
    if current.isEmpty { return committed }
    if committed.isEmpty { return current }
    return "\(committed) \(current)"
}

/// Heuristic detection of a legacy-tier phrase-segment boundary. `SFSpeechRecognizer` gives
/// NO explicit signal for this (confirmed empirically — see the streaming section's doc
/// comment): after a pause, `bestTranscription` just silently starts over on the next
/// non-final callback, with the task still `.running` and `isFinal` never firing early. So
/// a reset is inferred from the text itself: a genuine reset shares almost none of the
/// previous leading characters and either shrinks OR arrives after a phrase-sized callback gap —
/// vs. an ordinary in-flight revision (e.g. digit re-grouping: `"Testing one"` →
/// `"Testing 12"`, observed empirically), which may shrink by a character or two but keeps
/// most of the previous text's prefix intact. Requiring both conditions (not just a length
/// decrease) is what tells the two apart; `< 0.5` was chosen because the empirical reset
/// cases measured ~0–5% shared prefix vs. ~70%+ for ordinary revisions — comfortably clear
/// of both.
func legacySegmentDidReset(previous: String, new: String, gapSeconds: TimeInterval?) -> Bool {
    guard !previous.isEmpty else { return false }
    let commonPrefixLen = zip(previous, new).prefix { $0 == $1 }.count
    let ratio = Double(commonPrefixLen) / Double(previous.count)
    let phraseGap = gapSeconds.map { $0 >= 0.65 } ?? false
    return ratio < 0.5 && (new.count < previous.count || phraseGap)
}

/// Begin a new system-STT utterance (per-tier). Returns 0 on success.
@_cdecl("smk_sys_stream_start")
public func smk_sys_stream_start() -> Int32 {
    if #available(macOS 26, *) {
        return sysStreamStartModern()
    }
    return sysStreamStartLegacy()
}

@available(macOS 26, *)
private func sysStreamStartModern() -> Int32 {
    switch runBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 1 }
        // Self-heal (finding #1): tear down any prior session BEFORE installing a new one — a
        // failed tail-flush on the Rust side can skip the normal `smk_sys_stream_finish` call,
        // leaking the previous continuation/analyzer/drain-task. Discard its (now-abandoned)
        // hypothesis; the caller is starting a fresh utterance.
        if let prior = sysStreamTakeModern() {
            logErr("smk_sys_stream_start: tearing down a leaked prior session")
            _ = await sysStreamTeardownModern(prior)
        }
        // Force `.volatileResults` on top of whatever the `.transcription` preset's own
        // reportingOptions already are: this streaming path NEEDS non-final results, and the
        // preset's live default couldn't be confirmed on this (Sequoia-only) box — union
        // guarantees it regardless of what the preset already includes. `.fastResults`
        // biases the transcriber toward responsiveness (faster, slightly less accurate
        // interim guesses) to match the ~6/sec cadence of the ANE/Parakeet streaming path.
        let base = SpeechTranscriber.Preset.transcription
        let transcriber = SpeechTranscriber(
            locale: SYS_LOCALE,
            transcriptionOptions: base.transcriptionOptions,
            reportingOptions: base.reportingOptions.union([.volatileResults, .fastResults]),
            attributeOptions: base.attributeOptions
        )
        try await sysEnsureModel(transcriber)
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let (stream, cont) = AsyncStream<AnalyzerInput>.makeStream()
        try await analyzer.start(inputSequence: stream)
        // Resolved ONCE here (finding #5) and cached on the session — see its doc comment.
        let targetFormat = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])

        let session = SysModernSession(
            analyzer: analyzer, transcriber: transcriber, continuation: cont, targetFormat: targetFormat)
        // Drain `transcriber.results` on its own detached task — independent of whichever
        // thread is inside `push`/`finish`, per the section doc comment. Naturally ends once
        // `finalizeAndFinishThroughEndOfInput()` completes the results sequence.
        session.drainTask = Task.detached {
            do {
                for try await result in transcriber.results {
                    session.recordResult(String(result.text.characters), isFinal: result.isFinal)
                }
            } catch {
                logErr("smk_sys_stream: results drain error: \(error)")
            }
        }

        sysStreamSetModern(session)
        return 0
    }) {
    case .success(let code): return code
    case .failure(let e):
        logErr("smk_sys_stream_start error: \(e)")
        return 1
    }
}

/// Tear down a leaked prior legacy run/request before installing a new one (finding #1's
/// legacy-tier counterpart to `sysStreamTakeModern`+`sysStreamTeardownModern`): cancel the old
/// task and end its request so `smk_sys_stream_start` is self-healing even when the previous
/// utterance's `finish` never ran (e.g. a failed tail-flush on the Rust side skipped the real
/// finalize call). Any callback still in flight for the old run is neutralized by the identity
/// check in `sysStreamStartLegacy`'s completion handler (finding #3) once the slot below is
/// cleared, so there's no need to block here waiting for it to actually stop.
private func sysStreamTeardownLegacy() {
    sysStream.lock.lock()
    let priorRequest = sysStream.legacyRequest
    let priorRun = sysStream.legacyRun
    sysStream.legacyRequest = nil
    sysStream.legacyRun = nil
    sysStream.lock.unlock()
    guard priorRun != nil else { return }
    priorRun?.task?.cancel()
    priorRequest?.endAudio()
}

private func sysStreamStartLegacy() -> Int32 {
    let recognizer: SFSpeechRecognizer
    switch legacyEnsureAuthorizedRecognizer() {
    case .success(let r): recognizer = r
    case .failure(let e):
        logErr("smk_sys_stream_start: \(e)")
        return 1
    }

    sysStreamTeardownLegacy()

    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = true  // the core fix vs. legacyTranscribe's batch request
    // Finding #2 originally set this on the theory that an unhinted recognizer autonomously
    // emits `isFinal=true` after a mid-utterance pause, silently ending the request. Empirical
    // testing (real paced speech across a pause — see the streaming section's doc comment)
    // showed that's NOT what happens: the task stays alive and `isFinal` never fires early;
    // instead `bestTranscription` silently resets per phrase segment, which
    // `legacySegmentDidReset` + `LegacyRun`'s committed/partial split now handle. `.dictation`
    // is still Apple's documented hint for long continuous input, so it stays — just doesn't
    // single-handedly fix this. Only the streaming request needs it — `legacyTranscribe`'s
    // batch request is genuinely one-shot.
    request.taskHint = .dictation

    let run = LegacyRun()
    run.recognizer = recognizer

    sysStream.lock.lock()
    sysStream.legacyRequest = request
    sysStream.legacyRun = run
    sysStream.lock.unlock()

    run.task = recognizer.recognitionTask(with: request) { result, error in
        if let result {
            if result.isFinal {
                // The task's own `bestTranscription` only ever covers its CURRENT phrase
                // segment (see the streaming section's doc comment) — `finalJoined` prepends
                // whatever `run` had already committed, same shape as
                // `SysModernSession.hypothesis()`. Reads `run`'s OWN committed text (not
                // `SysStreamState`'s shared pointers, which `smk_sys_stream_finish` already
                // clears before this callback can fire) — see `LegacyRun`'s doc comment.
                run.finish(text: run.finalJoined(result.bestTranscription.formattedString), error: nil)
            } else {
                // Finding #3: a straggler callback from a cancelled/replaced run must not
                // clobber the CURRENTLY installed run's partial — check identity under the
                // same lock before writing. (`recordPartial` only ever mutates `run`'s OWN
                // fields now, so this is a defensive skip rather than the sole correctness
                // mechanism — see `LegacyRun`'s doc comment.)
                sysStream.lock.lock()
                let isCurrent = sysStream.legacyRun === run
                sysStream.lock.unlock()
                if isCurrent {
                    run.recordPartial(result.bestTranscription.formattedString)
                }
            }
        } else if let error {
            run.finish(
                text: legacyIsNoSpeech(error) ? "" : nil,
                error: legacyIsNoSpeech(error) ? nil : error)
        }
    }
    return 0
}

/// Convert `buffer` to the analyzer's preferred format if needed (mirrors `sysTranscribe`'s
/// own conversion step); `nil` means "use `buffer` as-is" (already compatible, or conversion
/// itself failed — logged, not fatal, since the analyzer may still accept the raw format).
/// Reads `session.targetFormat`, resolved ONCE at session start (finding #5) — `sysConvert`
/// itself is synchronous, so this no longer needs the `runBlocking` round-trip that used to
/// re-derive the format on every single push call.
@available(macOS 26, *)
private func sysStreamConvert(_ buffer: AVAudioPCMBuffer, for session: SysModernSession) -> AVAudioPCMBuffer? {
    guard let target = session.targetFormat, target != buffer.format else { return nil }
    do {
        return try sysConvert(buffer, to: target)
    } catch {
        logErr("smk_sys_stream_push: convert failed, using raw buffer: \(error)")
        return nil
    }
}

/// Feed a 16 kHz mono chunk; hand back the running hypothesis-so-far.
@_cdecl("smk_sys_stream_push")
public func smk_sys_stream_push(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    let audio = samples.map { Array(UnsafeBufferPointer(start: $0, count: n)) } ?? []
    guard sampleRate > 0, let buffer = sysMakeBuffer(audio, sampleRate: Double(sampleRate)) else {
        logErr("smk_sys_stream_push: bad audio")
        return 1
    }

    if #available(macOS 26, *) {
        sysStream.lock.lock()
        let session = sysStream.modern as? SysModernSession
        sysStream.lock.unlock()
        guard let session else {
            logErr("smk_sys_stream_push: not started")
            return 2
        }
        let toYield = sysStreamConvert(buffer, for: session) ?? buffer
        session.continuation.yield(AnalyzerInput(buffer: toYield))
        let text = session.hypothesis()
        text.withCString { cb?(ctx, $0) }
        return 0
    }

    sysStream.lock.lock()
    let request = sysStream.legacyRequest
    let run = sysStream.legacyRun
    sysStream.lock.unlock()
    guard let request, let run else {
        logErr("smk_sys_stream_push: not started")
        return 2
    }
    let text = run.hypothesis()
    request.append(buffer)  // NOT endAudio() yet — that's finish()'s job
    text.withCString { cb?(ctx, $0) }
    return 0
}

/// Flush the stream and return the final transcript.
@_cdecl("smk_sys_stream_finish")
public func smk_sys_stream_finish(
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SmkStrCb?
) -> Int32 {
    if #available(macOS 26, *) {
        sysStream.lock.lock()
        let session = sysStream.modern as? SysModernSession
        sysStream.modern = nil
        sysStream.lock.unlock()
        guard let session else {
            "".withCString { cb?(ctx, $0) }
            return 0
        }
        // `sysStreamTeardownModern` (finding #4) finalizes the analyzer, drains any results
        // still in flight (including the final one), and bounds the whole thing with a timeout
        // so a wedged `SpeechAnalyzer` can't hang the helper forever — mirroring the legacy
        // tier's explicit 30s bound just below. The closure never throws, so this is always
        // `.success`; `?? ""` is just defense against a stdlib-mandated `Result` shape.
        let text = (try? runBlocking({ await sysStreamTeardownModern(session) }).get()) ?? ""
        text.withCString { cb?(ctx, $0) }
        return 0
    }

    sysStream.lock.lock()
    let request = sysStream.legacyRequest
    let run = sysStream.legacyRun
    sysStream.legacyRequest = nil
    sysStream.legacyRun = nil
    sysStream.lock.unlock()
    guard let request, let run else {
        "".withCString { cb?(ctx, $0) }
        return 0
    }
    request.endAudio()
    // All audio was already fed incrementally as it arrived, so by `endAudio()` there's little
    // left for the recognizer to catch up on — a flat, generous bound plays the same role as
    // `legacyTranscribe`'s duration-based margin without needing a tracked utterance length here.
    if run.sem.wait(timeout: .now() + .seconds(30)) == .timedOut {
        run.task?.cancel()
        // Timeout fallback: hand back whatever committed+current-segment concatenation `run`
        // has accumulated so far rather than nothing — mirrors `sysStreamTeardownModern`'s
        // timeout path returning `session.hypothesis()`.
        run.finish(text: run.hypothesis(), error: nil)
    }
    if let text = run.text {
        text.withCString { cb?(ctx, $0) }
        return 0
    }
    logErr("smk_sys_stream_finish error: \(run.error ?? SmkError.noResult)")
    return 1
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
