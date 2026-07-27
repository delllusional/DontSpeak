import Foundation

public struct StatusPoll<Snapshot: Sendable>: Sendable {
    public let snapshot: Snapshot
    public let seq: UInt64
    public let running: Bool

    public init(snapshot: Snapshot, seq: UInt64, running: Bool) {
        self.snapshot = snapshot
        self.seq = seq
        self.running = running
    }
}

/// Runs blocking status waits on a raw thread, keeping the constructing caller responsive.
public final class StatusPump<Snapshot: Sendable>: @unchecked Sendable {
    private let thread: Thread

    public init(
        name: String,
        wait: @escaping @Sendable (UInt64) -> StatusPoll<Snapshot>,
        deliver: @escaping @Sendable (Snapshot) -> Void,
        finish: @escaping @Sendable () -> Void = {}
    ) {
        let thread = Thread {
            var since: UInt64 = 0
            var delivered = false
            var lastRunning = true
            while !Thread.current.isCancelled {
                let poll = wait(since)
                if statusShouldYield(
                    delivered: delivered,
                    seq: poll.seq,
                    since: since,
                    running: poll.running,
                    lastRunning: lastRunning
                ) {
                    deliver(poll.snapshot)
                    delivered = true
                    lastRunning = poll.running
                }
                since = poll.seq
                if !poll.running {
                    Thread.sleep(forTimeInterval: 0.4)
                }
            }
            finish()
        }
        thread.name = name
        self.thread = thread
        thread.start()
    }

    public func cancel() {
        thread.cancel()
    }

    deinit {
        thread.cancel()
    }
}
