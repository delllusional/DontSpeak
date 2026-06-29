//  TrayAnimator.swift
//
//  Animates the menu-bar icon so state changes aren't a jerky instant swap. Two effects,
//  combined:
//    • a ~0.22 s CROSSFADE on every state change (idle ↔ recording ↔ speaking ↔ mute) —
//      kills the jerk;
//    • a gentle BREATHING pulse (opacity 0.55…1.0) while recording/speaking — the live
//      cue, like the system microphone indicator.
//
//  It produces `NSImage` FRAMES that the bare `MenuBarExtra` label renders. The label stays
//  a modifier-free `Image(nsImage:)` (so the status item keeps hugging the icon — the whole
//  reason we can't animate it with SwiftUI/`symbolEffect` modifiers); we just swap the image
//  every frame. Driven off the @Observable `Core`: a `withObservationTracking` loop re-arms
//  on each tray-relevant change, and a 30 fps timer runs ONLY while there's something to
//  animate (a crossfade in flight or an active breathing state) — idle is static, no timer.

import AppKit
import Observation
import QuartzCore

@MainActor @Observable
final class TrayAnimator {
    /// The current frame the menu-bar label shows.
    private(set) var image: NSImage

    // `unowned`, not a strong `let`: Core owns this animator (strong `trayAnimator`),
    // so a strong back-reference here forms a Core ↔ TrayAnimator retain cycle. Core
    // outlives the animator (it creates it in its own init and is the app-lifetime
    // singleton), so `unowned` is safe and keeps non-optional access with no behaviour
    // change — it just lets the pair deallocate if Core is ever torn down.
    @ObservationIgnored private unowned let core: Core
    @ObservationIgnored private var timer: Timer?
    /// Crossfade endpoints (crossfade-SAFE images — see `TrayState.crossfadeImage`) + start
    /// time; `crossfading` is false once it completes.
    @ObservationIgnored private var fromImage: NSImage
    @ObservationIgnored private var toImage: NSImage
    /// The REAL image to rest on once animation stops (idle keeps its live-tinting template).
    @ObservationIgnored private var settledImage: NSImage
    @ObservationIgnored private var crossfadeStart: CFTimeInterval = 0
    @ObservationIgnored private var crossfading = false
    /// True while the active (recording/speaking) state should pulse.
    @ObservationIgnored private var breathing = false
    /// When the current breath began — the phase is anchored to it so a breath always STARTS
    /// at its peak (full pill, where a crossfade leaves it) and eases down, never jumping in
    /// from an arbitrary point in the sine cycle.
    @ObservationIgnored private var breatheStart: CFTimeInterval = 0
    /// The state/mute currently SHOWN, to detect real changes + pick the crossfade-from image.
    @ObservationIgnored private var shownState: TrayState
    @ObservationIgnored private var shownMuted: Bool

    private let crossfadeDur: CFTimeInterval = 0.22
    /// Full breathe cycle = 2.4 s (1.2 s in + 1.2 s out), matching the dictation overlay's
    /// `.easeInOut(duration: 1.2).repeatForever(autoreverses: true)` glow. The sine envelope
    /// below is the same ease-in/out-at-the-extremes feel — a clean fill-opacity pulse.
    private let breatheDur: CFTimeInterval = 2.4
    private let fps: CFTimeInterval = 1.0 / 30.0

    init(core: Core) {
        self.core = core
        let state = TrayState.current(core)
        let muted = core.activity.muted
        let img = state.image(muted: muted)
        image = img
        fromImage = img
        toImage = img
        settledImage = img
        shownState = state
        shownMuted = muted
        breathing = TrayState.animated(core)
        if breathing { breatheStart = CACurrentMediaTime() }
        observe()
        updateTimer()
    }

    /// Re-arm tracking of the tray-relevant reads; fire `sync` on any change.
    private func observe() {
        withObservationTracking {
            _ = Self.key(core)   // reads activity.{sttActive,ttsActive,muted,trayIndicator,engineRunning}
        } onChange: {
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.sync()
                self.observe()
            }
        }
    }

    /// A change landed: crossfade between crossfade-SAFE images (idle rendered non-template so
    /// the glyph can't flash black), from the state we were showing to the new one.
    private func sync() {
        let newState = TrayState.current(core)
        let newMuted = core.activity.muted
        let wasBreathing = breathing
        breathing = TrayState.animated(core)
        settledImage = newState.image(muted: newMuted)
        if newState != shownState || newMuted != shownMuted {
            fromImage = shownState.crossfadeImage(muted: shownMuted)
            toImage = newState.crossfadeImage(muted: newMuted)
            crossfadeStart = CACurrentMediaTime()
            crossfading = true
            shownState = newState
            shownMuted = newMuted
            // (`breatheStart` is anchored when this crossfade completes, in `tick`.)
        } else if !crossfading {
            // No state change and no crossfade in flight. If breathing JUST turned on (e.g. a
            // `tray_indicator` flip to the `_animated` form for the same colored state), anchor
            // the breath to NOW so it starts at its peak (full pill) — without this the next
            // `tick` would read a stale `breatheStart` and jump in at an arbitrary sine phase.
            if breathing && !wasBreathing { breatheStart = CACurrentMediaTime() }
            image = settledImage
        }
        updateTimer()
    }

    /// Run the 30 fps timer iff there's a crossfade in flight or an active breathing state.
    private func updateTimer() {
        let needed = crossfading || breathing
        if needed, timer == nil {
            // `.common` so the icon keeps animating while its own menu is open (menu tracking
            // runs the run loop in a non-default mode).
            // `[weak self]` MUST be on the OUTER timer block: the timer is a stored property, so
            // `self → timer → block → self` is a retain cycle if the block holds `self` strongly.
            // A `[weak self]` on only the inner `assumeIsolated` closure doesn't help — the outer
            // block still has to capture `self` strongly to form it. Capturing weakly out here
            // breaks the cycle (and `tick` simply no-ops if `self` is gone).
            let t = Timer(timeInterval: fps, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.tick() }
            }
            RunLoop.main.add(t, forMode: .common)
            timer = t
        } else if !needed, timer != nil {
            timer?.invalidate()
            timer = nil
            // Rest on the REAL image (restores the idle template so the bar tints it live).
            image = settledImage
        }
    }

    private func tick() {
        let now = CACurrentMediaTime()
        // A state change is mid-crossfade: blend the whole icons (no breathing yet).
        if crossfading {
            let t = min(1, (now - crossfadeStart) / crossfadeDur)
            image = (t >= 1) ? toImage : Self.blend(fromImage, toImage, CGFloat(t))
            if t >= 1 {
                crossfading = false
                // Begin the breath here, from the full pill the crossfade just settled on.
                if breathing { breatheStart = now } else { updateTimer() }
            }
            return
        }
        // Steady active state: breathe ONLY the pill (the glyph stays fully opaque). Anchored to
        // `breatheStart` with a +π/2 phase so it STARTS at the peak (full) and eases down — no
        // jump in from an arbitrary sine phase.
        if breathing {
            let phase = (sin((now - breatheStart) / breatheDur * 2 * .pi + .pi / 2) + 1) / 2   // starts at 1
            let pillAlpha = 0.725 + 0.275 * CGFloat(phase)           // 0.725…1.0 (half the dip from full)
            image = TrayState.current(core).breathingImage(muted: core.activity.muted, pillAlpha: pillAlpha)
        } else {
            image = toImage
            updateTimer()   // nothing left to animate → stop + settle
        }
    }

    /// Identity string for the tracked tray reads — the observation closure reads it so a
    /// change to any of those flags re-fires `sync`.
    private static func key(_ core: Core) -> String {
        // Include `animated` so a tray_indicator change that flips a state's static/animated
        // form (same colored state) still re-syncs the breathing.
        "\(TrayState.current(core))-\(core.activity.muted)-\(TrayState.animated(core))"
    }

    /// Composite `a` (fraction 1−t) under `b` (fraction t) — the crossfade frame. Non-template
    /// (it carries the active states' color); only a fully-settled idle frame stays template.
    private static func blend(_ a: NSImage, _ b: NSImage, _ t: CGFloat) -> NSImage {
        let out = NSImage(size: b.size, flipped: false) { rect in
            a.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1 - t)
            b.draw(in: rect, from: .zero, operation: .sourceOver, fraction: t)
            return true
        }
        out.isTemplate = false
        return out
    }
}
