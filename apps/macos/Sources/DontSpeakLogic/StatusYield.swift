/// Whether the status producer should push this snapshot to the UI.
///
/// The producer blocks in `WaitModelStatus`, which returns on a ~1 s timeout with an
/// UNCHANGED `seq` when idle. Re-yielding would churn every `@Observable` reader ~1×/s.
/// Yield when something actually changed:
///   - `!delivered` — first sample, always;
///   - `seq != since` — engine status gate advanced;
///   - `running != lastRunning` — `engineRunning` is an EXTERNAL pidfile/launchd probe
///     NOT in `seq`; a stop freezes `seq` and would leave a stale "running" menu-bar
///     dot if gated on `seq` alone.
public func statusShouldYield(
    delivered: Bool,
    seq: UInt64,
    since: UInt64,
    running: Bool,
    lastRunning: Bool
) -> Bool {
    !delivered || seq != since || running != lastRunning
}
