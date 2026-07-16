// Live Logs tab feed: background Thread blocks in `ds_logs_wait` → AsyncStream → main actor.
// Same producer/consumer shape as `Core.startStatusProducer` — FFI wait must not run on a
// Task (cooperative pool). Scoped to LogView appear/disappear (no work while tab closed).

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

    /// Immediate disk read, then blocking wait loop — both off the main actor.
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

    /// Clear + re-prime on a worker so disk work never blocks the main actor.
    /// Live watcher may also publish; newest-one buffer makes the double yield harmless.
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
