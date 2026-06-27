//  StatusYield.swift
//
//  Pure, dependency-free status-producer logic, split into its own target so it is
//  unit-testable without linking the app's Rust FFI staticlib or any system framework.

/// Decide whether the status producer should yield this snapshot to the UI.
///
/// The producer blocks in the engine's `WaitModelStatus`, which returns on a ~1 s timeout
/// with an UNCHANGED `seq` when nothing changed. Re-yielding that identical snapshot would
/// re-run `apply` and churn every `@Observable` reader (menu-bar label, open windows, the
/// TrayAnimator chain) ~1×/s forever while idle. So yield only when something actually
/// changed:
///   - `!delivered` — the very first sample, always delivered;
///   - `seq != since` — the daemon's status gate advanced (a real status change);
///   - `running != lastRunning` — `engineRunning` flipped. This is an EXTERNAL pidfile /
///     launchd probe NOT carried in the gate `seq`, so a stop/crash freezes `seq` and the
///     down transition would be missed if we gated on `seq` alone — leaving the menu-bar
///     dot a stale "running" until a manual refresh. Reacting to the flip fixes that while
///     preserving the idle dedup.
public func statusShouldYield(
    delivered: Bool,
    seq: UInt64,
    since: UInt64,
    running: Bool,
    lastRunning: Bool
) -> Bool {
    !delivered || seq != since || running != lastRunning
}
