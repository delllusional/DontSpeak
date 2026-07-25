// libdontspeak_sys -- C ABI over Apple on-device System STT (en-US). Frameworks only
// (builds with bare swiftc on every arch). See dontspeak_sys.h. 0 = success. Serial
// helper calls; locks guard shared state.
import AVFoundation
import Foundation
import Speech
import os

// MARK: - async -> blocking bridge (Rust worker thread only; Swift Task deadlocks the pool)

private final class SysBox<T>: @unchecked Sendable { var value: Result<T, Error>? }

/// Park calling thread until `op` completes. C entry from Rust worker only.
private func sysRunBlocking<T>(_ op: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let sem = DispatchSemaphore(value: 0)
    let box = SysBox<T>()
    Task.detached {
        do { box.value = .success(try await op()) } catch { box.value = .failure(error) }
        sem.signal()
    }
    sem.wait()
    return box.value ?? .failure(SysShimError.noResult)
}

enum SysShimError: Error {
    case noResult, badAudio
    case sysUnavailable(String)
}

// MARK: - borrowed-result callback (see dontspeak_shim.h)
// Success: fire cb once with borrowed buffer; Rust copies out. Status is the C return.
public typealias SysStrCb = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void


// MARK: - System STT (on-device en-US)
// 26+: SpeechAnalyzer (no Speech-Rec TCC; model download on enable).
// 14–25: SFSpeechRecognizer + requiresOnDeviceRecognition; private OperationQueue
// (main-queue default deadlocked the helper); needs Speech-Rec TCC.
// Status: 0 ready, 1 preparing, 2 no on-device locale, 3 too old, 4 permission denied.

private let SYS_LOCALE = Locale(identifier: "en-US")

private func sysMakeBuffer(_ samples: [Float], sampleRate: Double) -> AVAudioPCMBuffer? {
    guard
        let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: sampleRate, channels: 1, interleaved: false),
        let buf = AVAudioPCMBuffer(
            pcmFormat: format, frameCapacity: AVAudioFrameCount(max(samples.count, 1)))
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

/// Legacy callbacks on a private queue (main-queue default deadlocked the helper).
private let legacyQueue: OperationQueue = {
    let q = OperationQueue()
    q.name = "dontspeak_sys.sysstt.legacy"
    q.maxConcurrentOperationCount = 1
    return q
}()

private func legacyRecognizer() -> SFSpeechRecognizer? {
    guard let r = SFSpeechRecognizer(locale: SYS_LOCALE) else { return nil }
    r.queue = legacyQueue
    return r
}

/// Legacy availability; preparing = permission not yet requested.
private func legacyAvailable() -> Int32 {
    guard let r = legacyRecognizer(), r.supportsOnDeviceRecognition else { return 2 }
    switch SFSpeechRecognizer.authorizationStatus() {
    case .authorized: return 0
    case .notDetermined: return 1
    default: return 4
    }
}

/// Blocking Speech-Rec TCC. Poll status (completion may be main queue, which never drains).
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
    // Stuck notDetermined → rc 4; later authorize can still succeed.
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else { return 4 }
    // supportsOnDeviceRecognition lies if Dictation is off (kLSRErrorDomain 201).
    // Smoke silence so enable fails here instead of a green-but-dead UI.
    switch legacyTranscribe([Float](repeating: 0, count: 3_200), sampleRate: 16_000) {
    case .success:
        return 0
    case .failure(let e):
        let ns = e as NSError
        sysLogErr("system STT smoke recognition failed: \(e)")
        return ns.domain == "kLSRErrorDomain" && ns.code == 201 ? 2 : 0
    }
}

/// Debug-only reset timing (never logs dictated text).
private let legacyResetLog = Logger(subsystem: "app.dontspeak.org", category: "legacy-stt")

/// Per-run recognition state. C thread parks on `sem`. finish is single-shot.
/// Streaming text lives on the run (not SysStreamState) so a replaced run cannot
/// corrupt another's committed/partial; isFinal can still read after shared pointers clear.
final class LegacyRun: @unchecked Sendable {
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
    /// Popup/final candidate; empty non-final callbacks ignored.
    private var settledPartial: String = ""
    private var lastPartialChangedAt: TimeInterval?

    func finish(text: String?, error: Error?) {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return }
        done = true
        self.text = text
        self.error = error
        sem.signal()
    }

    func hypothesis() -> String {
        textLock.lock()
        defer { textLock.unlock() }
        return legacyJoin(committedText, settledPartial)
    }

    /// No-speech after a valid partial still finalizes that text.
    func finishNoSpeech() {
        finish(text: finalJoined("", preserveStrictPrefix: false), error: nil)
    }

    /// Non-final partial; on phrase reset, commit settled segment.
    func recordPartial(_ newText: String) {
        // Empty non-final between hypotheses — ignore (keeps timing).
        guard !newText.isEmpty else { return }

        textLock.lock()
        defer { textLock.unlock() }
        let now = ProcessInfo.processInfo.systemUptime
        let timing = legacyPartialTiming(
            previous: latestPartial,
            new: newText,
            lastChangedAt: lastPartialChangedAt,
            now: now)
        let gap = timing.gapSeconds
        if legacySegmentDidReset(previous: latestPartial, new: newText, gapSeconds: gap),
            !latestPartial.isEmpty
        {
            // Numeric only (.public); never dictated text.
            if let gap {
                legacyResetLog.debug(
                    "legacy STT phrase reset: gapSeconds=\(gap, format: .fixed(precision: 3), privacy: .public)"
                )
            } else {
                legacyResetLog.debug("legacy STT phrase reset: gapSeconds=nil")
            }
            committedText = legacyJoin(committedText, settledPartial)
            settledPartial = newText
        } else {
            settledPartial = systemSettledSegment(
                latestPartial: settledPartial,
                incomingSegment: newText,
                preserveStrictPrefix: false)
        }
        latestPartial = newText
        lastPartialChangedAt = timing.lastChangedAt
    }

    func finalJoined(_ finalSegment: String, preserveStrictPrefix: Bool) -> String {
        textLock.lock()
        defer { textLock.unlock() }
        let settled = systemSettledSegment(
            latestPartial: settledPartial,
            incomingSegment: finalSegment,
            preserveStrictPrefix: preserveStrictPrefix)
        return legacyJoin(committedText, settled)
    }
}

/// kAFAssistantErrorDomain 1110 no-speech → empty transcript (success).
private func legacyIsNoSpeech(_ error: Error) -> Bool {
    let e = error as NSError
    return e.domain == "kAFAssistantErrorDomain" && e.code == 1110
}

/// Auth+recognizer gate for batch and streaming legacy.
private func legacyEnsureAuthorizedRecognizer() -> Result<SFSpeechRecognizer, Error> {
    if SFSpeechRecognizer.authorizationStatus() == .notDetermined {
        _ = legacyAuthorize()
    }
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else {
        return .failure(SysShimError.sysUnavailable("speech recognition permission not granted"))
    }
    guard let recognizer = legacyRecognizer(), recognizer.supportsOnDeviceRecognition else {
        return .failure(SysShimError.sysUnavailable("no on-device recognition for en-US"))
    }
    return .success(recognizer)
}

/// Legacy batch; bounded wait so a wedged recognizer cannot hang the helper.
private func legacyTranscribe(_ samples: [Float], sampleRate: Double) -> Result<String, Error> {
    let recognizer: SFSpeechRecognizer
    switch legacyEnsureAuthorizedRecognizer() {
    case .success(let r): recognizer = r
    case .failure(let e): return .failure(e)
    }
    guard sampleRate > 0, let buffer = sysMakeBuffer(samples, sampleRate: sampleRate) else {
        return .failure(SysShimError.badAudio)
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

    // Faster than real time; audio length + 30s is generous.
    let margin = Int(Double(samples.count) / sampleRate) + 30
    if run.sem.wait(timeout: .now() + .seconds(margin)) == .timedOut {
        run.task?.cancel()
        run.finish(text: nil, error: SysShimError.sysUnavailable("recognition timed out"))
    }
    if let text = run.text { return .success(text) }
    return .failure(run.error ?? SysShimError.noResult)
}

// MARK: C entry points (both tiers)

/// Non-prompting availability for model-status poll (1 → preparing).
@_cdecl("ds_sys_available")
public func ds_sys_available() -> Int32 {
    guard #available(macOS 26, *) else { return legacyAvailable() }
    switch sysRunBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 2 }
        return await sysModelInstalled() ? 0 : 1
    }) {
    case .success(let code): return code
    default: return 2
    }
}

/// Blocking enable: 26+ model download; <26 Speech-Rec TCC. 0 = ready.
@_cdecl("ds_sys_authorize")
public func ds_sys_authorize() -> Int32 {
    guard #available(macOS 26, *) else { return legacyAuthorize() }
    switch sysRunBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 2 }
        let transcriber = SpeechTranscriber(locale: SYS_LOCALE, preset: .transcription)
        try await sysEnsureModel(transcriber)
        return 0
    }) {
    case .success(let code): return code
    case .failure(let e):
        sysLogErr("ds_sys_authorize error: \(e)")
        return 2
    }
}

/// On-device batch; cb borrows text. Empty → "".
@_cdecl("ds_sys_transcribe")
public func ds_sys_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SysStrCb?
) -> Int32 {
    guard let cb else { return 4 }
    guard let samples, n > 0 else {
        "".withCString { cb(ctx, $0) }
        return 0
    }
    let pcm = Array(UnsafeBufferPointer(start: samples, count: n))
    let outcome: Result<String, Error>
    if #available(macOS 26, *) {
        outcome = sysRunBlocking({ try await sysTranscribe(pcm, sampleRate: Double(sampleRate)) })
    } else {
        outcome = legacyTranscribe(pcm, sampleRate: Double(sampleRate))
    }
    switch outcome {
    case .success(let text):
        text.withCString { cb(ctx, $0) }
        return 0
    case .failure(let e):
        sysLogErr("ds_sys_transcribe error: \(e)")
        return 1
    }
}

@available(macOS 26, *)
private func sysTranscribe(_ samples: [Float], sampleRate: Double) async throws -> String {
    let transcriber = SpeechTranscriber(locale: SYS_LOCALE, preset: .transcription)
    try await sysEnsureModel(transcriber)
    let analyzer = SpeechAnalyzer(modules: [transcriber])

    guard let inBuf = sysMakeBuffer(samples, sampleRate: sampleRate) else { return "" }
    var buffer = inBuf
    if let target = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]),
        target != inBuf.format,
        let converted = try sysConvert(inBuf, to: target)
    {
        buffer = converted
    }

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

/// Converter feed: whole buffer once then EOS (avoids capturing non-Sendable buffer).
private final class ConvertFeed: @unchecked Sendable {
    let buffer: AVAudioPCMBuffer
    var done = false
    init(_ b: AVAudioPCMBuffer) { buffer = b }
}

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

// MARK: - System STT streaming (start/push/finish like ds_mlx_asr_stream_*)
// 26+: persistent SpeechAnalyzer; drain volatile + final into committed/latest.
// <26: one request/task with partials=true. No phrase-boundary signal — bestTranscription
// resets after pauses; legacySegmentDidReset + LegacyRun commit segments.

private func systemSpeechWords(_ text: String) -> [String] {
    text.lowercased().split { !$0.isLetter && !$0.isNumber }.map(String.init)
}

/// Prefer nonempty prior; optional strict-prefix keep (macOS 15 final-truncation).
func systemSettledSegment(
    latestPartial: String,
    incomingSegment: String,
    preserveStrictPrefix: Bool
) -> String {
    let partialWords = systemSpeechWords(latestPartial)
    guard !partialWords.isEmpty else { return incomingSegment }

    let incomingWords = systemSpeechWords(incomingSegment)
    if incomingWords.isEmpty
        || (preserveStrictPrefix
            && partialWords.count > incomingWords.count
            && partialWords.starts(with: incomingWords))
    {
        return latestPartial
    }
    return incomingSegment
}

/// macOS 15: final can be a strict prefix of last complete partial.
private var legacyNeedsStrictPrefixFinalWorkaround: Bool {
    ProcessInfo.processInfo.operatingSystemVersion.majorVersion == 15
}

/// 26+ session; type-erased as Any? on SysStreamState so outer class needs no @available.
@available(macOS 26, *)
private final class SysModernSession: @unchecked Sendable {
    let lock = NSLock()
    let analyzer: SpeechAnalyzer
    let transcriber: SpeechTranscriber
    let continuation: AsyncStream<AnalyzerInput>.Continuation
    /// Format resolved once at start (avoids per-push async re-derive).
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

    func hypothesis() -> String {
        lock.lock()
        defer { lock.unlock() }
        if latestVolatileText.isEmpty { return committedText }
        if committedText.isEmpty { return latestVolatileText }
        return "\(committedText) \(latestVolatileText)"
    }

    /// Sync so drain Task can take NSLock (unavailable from async).
    func recordResult(_ text: String, isFinal: Bool) {
        lock.lock()
        defer { lock.unlock() }
        if isFinal {
            let settled = systemSettledSegment(
                latestPartial: latestVolatileText,
                incomingSegment: text,
                preserveStrictPrefix: false)
            if !settled.isEmpty {
                committedText =
                    committedText.isEmpty ? settled : "\(committedText) \(settled)"
            }
            latestVolatileText = ""
        } else {
            latestVolatileText = systemSettledSegment(
                latestPartial: latestVolatileText,
                incomingSegment: text,
                preserveStrictPrefix: false)
        }
    }
}

@available(macOS 26, *)
private func sysStreamSetModern(_ session: SysModernSession) {
    sysStream.lock.lock()
    defer { sysStream.lock.unlock() }
    sysStream.modern = session
}

/// Pop modern session (tear down before replace if finish was skipped).
@available(macOS 26, *)
private func sysStreamTakeModern() -> SysModernSession? {
    sysStream.lock.lock()
    defer { sysStream.lock.unlock() }
    let prior = sysStream.modern as? SysModernSession
    sysStream.modern = nil
    return prior
}

/// Modern finish timeout (mirrors legacy 30s bound).
private let sysStreamModernFinishTimeoutSeconds: Double = 30

/// Resume once (finish vs timeout race); extra resumes dropped.
private final class SingleShotContinuation<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    /// True iff this call performed the resume.
    func resumeOnce(_ continuation: CheckedContinuation<T, Never>, with value: T) -> Bool {
        lock.lock()
        let already = done
        done = true
        lock.unlock()
        if !already { continuation.resume(returning: value) }
        return !already
    }
}

/// Finish + drain modern session, bounded. Race finish vs timer (TaskGroup cannot
/// bound cancel-ignoring work). Timeout returns partial hypothesis.
@available(macOS 26, *)
private func sysStreamTeardownModern(_ session: SysModernSession) async -> String {
    session.continuation.finish()
    let gate = SingleShotContinuation<String>()
    return await withCheckedContinuation { (cont: CheckedContinuation<String, Never>) in
        Task.detached {
            do {
                try await session.analyzer.finalizeAndFinishThroughEndOfInput()
            } catch {
                sysLogErr("sysStreamTeardownModern: finalize failed: \(error)")
            }
            _ = await session.drainTask?.value
            _ = gate.resumeOnce(cont, with: session.hypothesis())
        }
        Task.detached {
            try? await Task.sleep(for: .seconds(sysStreamModernFinishTimeoutSeconds))
            if gate.resumeOnce(cont, with: session.hypothesis()) {
                sysLogWarn("sysStreamTeardownModern: timed out after \(sysStreamModernFinishTimeoutSeconds)s")
            }
        }
    }
}

private final class SysStreamState: @unchecked Sendable {
    let lock = NSLock()
    // Legacy (<26); committed/segment split lives on legacyRun.
    var legacyRun: LegacyRun?
    var legacyRequest: SFSpeechAudioBufferRecognitionRequest?
    // Modern (26+): type-erased SysModernSession.
    var modern: Any?
}
private let sysStream = SysStreamState()

private func legacyJoin(_ committed: String, _ current: String) -> String {
    if current.isEmpty { return committed }
    if committed.isEmpty { return current }
    return "\(committed) \(current)"
}

/// Gap since last *changed* hyp. Duplicate hyps at a boundary must not refresh the
/// timestamp (~2s pause looked like 0.3s). 0.65s threshold for low-prefix revisions.
func legacyPartialTiming(
    previous: String,
    new: String,
    lastChangedAt: TimeInterval?,
    now: TimeInterval
) -> (gapSeconds: TimeInterval?, lastChangedAt: TimeInterval?) {
    let gap = lastChangedAt.map { now - $0 }
    return (gap, new == previous ? lastChangedAt : now)
}

/// Phrase-segment boundary (SFSpeechRecognizer has no explicit signal after pauses).
/// Reset iff shared-prefix ratio < 0.5 AND (shrink OR gap ≥ 0.65s). Ratio separates
/// genuine resets (~0–5% prefix) from in-phrase revisions like digit regrouping (~70%+).
func legacySegmentDidReset(previous: String, new: String, gapSeconds: TimeInterval?) -> Bool {
    guard !previous.isEmpty else { return false }
    let commonPrefixLen = zip(previous, new).prefix { $0 == $1 }.count
    let ratio = Double(commonPrefixLen) / Double(previous.count)
    let phraseGap = gapSeconds.map { $0 >= 0.65 } ?? false
    return ratio < 0.5 && (new.count < previous.count || phraseGap)
}

@_cdecl("ds_sys_stream_start")
public func ds_sys_stream_start() -> Int32 {
    if #available(macOS 26, *) {
        return sysStreamStartModern()
    }
    return sysStreamStartLegacy()
}

@available(macOS 26, *)
private func sysStreamStartModern() -> Int32 {
    switch sysRunBlocking({ () -> Int32 in
        guard await sysLocaleSupported() else { return 1 }
        if let prior = sysStreamTakeModern() {
            sysLogWarn("ds_sys_stream_start: tearing down a leaked prior session")
            _ = await sysStreamTeardownModern(prior)
        }
        // Union preset with volatile+fastResults (~6/sec partials).
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
        let targetFormat = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])

        let session = SysModernSession(
            analyzer: analyzer, transcriber: transcriber, continuation: cont, targetFormat: targetFormat)
        // Detached drain ends when finalize completes.
        session.drainTask = Task.detached {
            do {
                for try await result in transcriber.results {
                    session.recordResult(String(result.text.characters), isFinal: result.isFinal)
                }
            } catch {
                sysLogErr("ds_sys_stream: results drain error: \(error)")
            }
        }

        sysStreamSetModern(session)
        return 0
    }) {
    case .success(let code): return code
    case .failure(let e):
        sysLogErr("ds_sys_stream_start error: \(e)")
        return 1
    }
}

/// Legacy teardown before replace; identity check drops straggler callbacks.
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
        sysLogErr("ds_sys_stream_start: \(e)")
        return 1
    }

    sysStreamTeardownLegacy()

    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = true
    // .dictation: continuous input, no auto-isFinal on pause; phrase resets via
    // legacySegmentDidReset. Streaming only — batch is one-shot.
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
                // bestTranscription is current segment only; finalJoined prepends committed.
                run.finish(
                    text: run.finalJoined(
                        result.bestTranscription.formattedString,
                        preserveStrictPrefix: legacyNeedsStrictPrefixFinalWorkaround),
                    error: nil)
            } else {
                // Drop straggler callbacks from replaced runs.
                sysStream.lock.lock()
                let isCurrent = sysStream.legacyRun === run
                sysStream.lock.unlock()
                if isCurrent {
                    run.recordPartial(result.bestTranscription.formattedString)
                }
            }
        } else if let error {
            let noSpeech = legacyIsNoSpeech(error)
            // No-speech after valid partial → keep partial (esp. macOS 15).
            if noSpeech {
                run.finishNoSpeech()
            } else {
                run.finish(text: nil, error: error)
            }
        }
    }
    return 0
}

/// Convert to session.targetFormat if needed; nil = use as-is.
@available(macOS 26, *)
private func sysStreamConvert(_ buffer: AVAudioPCMBuffer, for session: SysModernSession) -> AVAudioPCMBuffer?
{
    guard let target = session.targetFormat, target != buffer.format else { return nil }
    do {
        return try sysConvert(buffer, to: target)
    } catch {
        sysLogWarn("ds_sys_stream_push: convert failed, using raw buffer: \(error)")
        return nil
    }
}

/// Feed 16 kHz mono chunk; cb gets running hypothesis.
@_cdecl("ds_sys_stream_push")
public func ds_sys_stream_push(
    _ samples: UnsafePointer<Float>?,
    _ n: Int,
    _ sampleRate: Int32,
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SysStrCb?
) -> Int32 {
    guard let cb else { return 4 }
    let audio = samples.map { Array(UnsafeBufferPointer(start: $0, count: n)) } ?? []
    guard sampleRate > 0, let buffer = sysMakeBuffer(audio, sampleRate: Double(sampleRate)) else {
        sysLogErr("ds_sys_stream_push: bad audio")
        return 1
    }

    if #available(macOS 26, *) {
        sysStream.lock.lock()
        let session = sysStream.modern as? SysModernSession
        sysStream.lock.unlock()
        guard let session else {
            sysLogErr("ds_sys_stream_push: not started")
            return 2
        }
        let toYield = sysStreamConvert(buffer, for: session) ?? buffer
        session.continuation.yield(AnalyzerInput(buffer: toYield))
        let text = session.hypothesis()
        text.withCString { cb(ctx, $0) }
        return 0
    }

    sysStream.lock.lock()
    let request = sysStream.legacyRequest
    let run = sysStream.legacyRun
    sysStream.lock.unlock()
    guard let request, let run else {
        sysLogErr("ds_sys_stream_push: not started")
        return 2
    }
    let text = run.hypothesis()
    request.append(buffer)  // endAudio is finish()'s job
    text.withCString { cb(ctx, $0) }
    return 0
}

/// Flush stream; cb gets final transcript.
@_cdecl("ds_sys_stream_finish")
public func ds_sys_stream_finish(
    _ ctx: UnsafeMutableRawPointer?,
    _ cb: SysStrCb?
) -> Int32 {
    guard let cb else { return 4 }
    if #available(macOS 26, *) {
        sysStream.lock.lock()
        let session = sysStream.modern as? SysModernSession
        sysStream.modern = nil
        sysStream.lock.unlock()
        guard let session else {
            "".withCString { cb(ctx, $0) }
            return 0
        }
        // Bounded finish+drain (30s); timeout returns partial.
        let text = (try? sysRunBlocking({ await sysStreamTeardownModern(session) }).get()) ?? ""
        text.withCString { cb(ctx, $0) }
        return 0
    }

    sysStream.lock.lock()
    let request = sysStream.legacyRequest
    let run = sysStream.legacyRun
    sysStream.legacyRequest = nil
    sysStream.legacyRun = nil
    sysStream.lock.unlock()
    guard let request, let run else {
        "".withCString { cb(ctx, $0) }
        return 0
    }
    request.endAudio()
    // Flat 30s (audio already fed incrementally).
    if run.sem.wait(timeout: .now() + .seconds(30)) == .timedOut {
        run.task?.cancel()
        run.finish(text: run.hypothesis(), error: nil)
    }
    if let text = run.text {
        text.withCString { cb(ctx, $0) }
        return 0
    }
    sysLogErr("ds_sys_stream_finish error: \(run.error ?? SysShimError.noResult)")
    return 1
}
