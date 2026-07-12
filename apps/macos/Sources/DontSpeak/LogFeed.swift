//  LogFeed.swift
//
//  Drives the Logs tab live while it's open: a dedicated background `Thread` blocks in
//  `ds_logs_wait` in a loop and yields each fresh tail into an `AsyncStream`, consumed by a
//  `Task` on the main actor — the CLIENT-SIDE analogue of `Core.startStatusProducer` (that one
//  blocks over the engine's IPC socket; this one blocks on an fs watch in THIS process, since
//  logs are read straight off disk here, same as `ds_logs_json`). Same shape as `Core`'s
//  producer/consumer split for the SAME reason: the FFI wait call blocks, so it must run on a
//  raw `Thread`, never a `Task` (which would starve the cooperative pool); the stream boundary
//  is what lets a non-Sendable `@Observable` class receive it safely under strict concurrency.
//  Scoped to `LogView`'s own lifetime (start on appear, stop on disappear) rather than the
//  whole app, since pushing logs while the tab is closed is pure waste.

import CDontSpeak
import DontSpeakLogic
import Foundation

@Observable @MainActor
final class LogFeed {
    private(set) var lines: [LogLine] = []
    private(set) var orderedSources: [String] = []

    @ObservationIgnored private var consumeTask: Task<Void, Never>?
    @ObservationIgnored private var continuation: AsyncStream<[LogLine]>.Continuation?
    @ObservationIgnored private nonisolated(unsafe) var pushThread: Thread?

    private static let maxBytes: UInt32 = 64 * 1024
    private static let timeoutMs: UInt32 = 2000

    /// Start with an immediate read on the producer thread, then enter the blocking-wait loop.
    /// Both FFI calls touch disk, so neither belongs on the main actor that handles tab input.
    func start() {
        let (stream, cont) = AsyncStream<[LogLine]>.makeStream(bufferingPolicy: .bufferingNewest(1))
        continuation = cont
        consumeTask = Task { [weak self] in
            for await decoded in stream {
                guard let self else { break }
                self.apply(decoded)
            }
        }
        let maxBytes = Self.maxBytes
        let timeoutMs = Self.timeoutMs
        let t = Thread {
            let initial = ffiDecode([LogLine].self) {
                ds_logs_json(maxBytes)
            } ?? []
            if Thread.current.isCancelled {
                cont.finish()
                return
            }
            cont.yield(initial)
            while !Thread.current.isCancelled {
                let decoded = ffiDecode([LogLine].self) {
                    ds_logs_wait(maxBytes, timeoutMs)
                } ?? []
                if Thread.current.isCancelled { break }
                cont.yield(decoded)
            }
            cont.finish()
        }
        t.name = "logs-push"
        pushThread = t
        t.start()
    }

    func stop() {
        pushThread?.cancel()
        pushThread = nil
        consumeTask?.cancel()
        consumeTask = nil
        continuation?.finish()
        continuation = nil
    }

    /// Clear and re-prime on a one-shot worker so the destructive file operation cannot block
    /// the main actor. The live watcher will normally publish the same result too; the stream's
    /// newest-one buffer makes that harmless while guaranteeing an immediate empty refresh.
    func clear() {
        guard let continuation else { return }
        let maxBytes = Self.maxBytes
        let t = Thread {
            ds_logs_clear()
            let decoded = ffiDecode([LogLine].self) { ds_logs_json(maxBytes) } ?? []
            continuation.yield(decoded)
        }
        t.name = "logs-clear"
        t.start()
    }

    private func apply(_ decoded: [LogLine]) {
        lines = decoded
        orderedSources = LogCatalog.distinctSources(decoded).filter { !$0.isEmpty }
    }
}
