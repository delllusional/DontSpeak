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

    /// Prime synchronously (mirrors `Core.init()`'s non-blocking prime before starting its
    /// producer thread), then start the blocking-wait loop.
    func start() {
        reloadNow()
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

    /// One-shot synchronous re-read (the initial prime in `start()`, and the Clear button's
    /// re-prime after `ds_logs_clear()`) — the SAME `ds_logs_json` call `start()` uses, kept
    /// in one place so the two paths can't drift.
    func reloadNow() {
        apply(ffiDecode([LogLine].self) { ds_logs_json(Self.maxBytes) } ?? [])
    }

    private func apply(_ decoded: [LogLine]) {
        lines = decoded
        orderedSources = LogCatalog.distinctSources(decoded).filter { !$0.isEmpty }
    }
}
