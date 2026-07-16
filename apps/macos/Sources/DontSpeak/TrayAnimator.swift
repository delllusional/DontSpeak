// Menu-bar icon frames: ~0.22s crossfade on state change + breathing while recording/speaking.
// Label stays a bare `Image(nsImage:)` (modifiers balloon the status item width). Timer at 30fps
// only while animating — idle is static.

import AppKit
import Observation
import QuartzCore

@MainActor @Observable
final class TrayAnimator {
    private(set) var image: NSImage

    // `unowned`: Core owns this animator strongly; a strong back-ref would cycle. Core is
    // app-lifetime and outlives us.
    @ObservationIgnored private unowned let core: Core
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var fromImage: NSImage
    @ObservationIgnored private var toImage: NSImage
    /// Real settle image (idle keeps live-tinting template).
    @ObservationIgnored private var settledImage: NSImage
    @ObservationIgnored private var crossfadeStart: CFTimeInterval = 0
    @ObservationIgnored private var crossfading = false
    @ObservationIgnored private var breathing = false
    /// Breath phase anchored so a breath starts at peak (full pill after crossfade).
    @ObservationIgnored private var breatheStart: CFTimeInterval = 0
    @ObservationIgnored private var shownState: TrayState
    @ObservationIgnored private var shownMuted: Bool

    private let crossfadeDur: CFTimeInterval = 0.22
    /// 2.4s full cycle — matches dictation overlay glow.
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

    private func observe() {
        withObservationTracking {
            _ = Self.key(core)
        } onChange: {
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.sync()
                self.observe()
            }
        }
    }

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
        } else if !crossfading {
            // Breathing-only flip (same color state, `_animated` form): re-anchor so we
            // don't jump in at an arbitrary sine phase.
            if breathing && !wasBreathing { breatheStart = CACurrentMediaTime() }
            image = settledImage
        }
        updateTimer()
    }

    private func updateTimer() {
        let needed = crossfading || breathing
        if needed, timer == nil {
            // `.common`: keep animating while the menu is open (non-default run-loop mode).
            // `[weak self]` on the OUTER block — timer is stored, so a strong capture cycles.
            let t = Timer(timeInterval: fps, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.tick() }
            }
            RunLoop.main.add(t, forMode: .common)
            timer = t
        } else if !needed, timer != nil {
            timer?.invalidate()
            timer = nil
            image = settledImage
        }
    }

    private func tick() {
        let now = CACurrentMediaTime()
        if crossfading {
            let t = min(1, (now - crossfadeStart) / crossfadeDur)
            image = (t >= 1) ? toImage : Self.blend(fromImage, toImage, CGFloat(t))
            if t >= 1 {
                crossfading = false
                if breathing { breatheStart = now } else { updateTimer() }
            }
            return
        }
        // Breathe only the pill; +π/2 phase starts at peak.
        if breathing {
            let phase = (sin((now - breatheStart) / breatheDur * 2 * .pi + .pi / 2) + 1) / 2
            let pillAlpha = 0.725 + 0.275 * CGFloat(phase)
            image = TrayState.current(core).breathingImage(muted: core.activity.muted, pillAlpha: pillAlpha)
        } else {
            image = toImage
            updateTimer()
        }
    }

    private static func key(_ core: Core) -> String {
        // Include `animated` so static↔animated flips for the same color still re-sync.
        "\(TrayState.current(core))-\(core.activity.muted)-\(TrayState.animated(core))"
    }

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
