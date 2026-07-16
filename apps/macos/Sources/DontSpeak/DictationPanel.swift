// Dictation confirm overlay: non-activating floating panel (partials + await confirm).
// Confirm/cancel are Caps in the engine — panel has no buttons and must not take focus
// so paste lands in the dictation target. Driven by Core.dictation via apply(...).

import AppKit
import SwiftUI

/// Overlay geometry shared by AppKit sizing + SwiftUI content.
private enum Overlay {
    /// Default width; user width persisted via OverlayWidth (edge drag).
    static let width: CGFloat = 460
    /// Resize bounds + side grab margin (resize vs move).
    static let minWidth: CGFloat = 280
    static let maxWidth: CGFloat = 900
    static let edgeMargin: CGFloat = 8
    static let corner: CGFloat = 18
    static let pad: CGFloat = 14
    static let fontSize: CGFloat = 16
    static let font: Font = .system(size: fontSize, weight: .medium)
    /// Resting single-line height (font + padding); controller anchors top here.
    static var restHeight: CGFloat {
        let f = NSFont.systemFont(ofSize: fontSize, weight: .medium)
        return ceil(f.ascender - f.descender + f.leading) + pad * 2
    }
}

/// Engine dictation state bound to the overlay view.
@Observable @MainActor
final class DictationModel {
    var text: String = ""
    var target: String?
    var awaiting: Bool = false
    var recording: Bool = false
    /// Editable paste target focused? False → warning glow.
    var hasTarget: Bool = true
    /// Engine `prompt_glow` — shared with Windows.
    var promptGlow: Bool = false
    /// User-resizable; drives content re-wrap.
    var width: CGFloat = Overlay.width
}

/// Borderless non-activating panel: mouse for drag, never key/main (paste stays on target).
private final class OverlayPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Persisted top-left (pill grows downward — top pin stays put).
private enum OverlayPosition {
    private static let keyX = "DictationOverlay.topLeftX"
    private static let keyY = "DictationOverlay.topLeftY"

    static var saved: NSPoint? {
        let d = UserDefaults.standard
        guard d.object(forKey: keyX) != nil, d.object(forKey: keyY) != nil else { return nil }
        return NSPoint(x: d.double(forKey: keyX), y: d.double(forKey: keyY))
    }

    static func save(_ topLeft: NSPoint) {
        let d = UserDefaults.standard
        d.set(Double(topLeft.x), forKey: keyX)
        d.set(Double(topLeft.y), forKey: keyY)
    }
}

/// Persisted width (independent of drop point).
private enum OverlayWidth {
    private static let key = "DictationOverlay.width"

    static var saved: CGFloat? {
        let d = UserDefaults.standard
        guard d.object(forKey: key) != nil else { return nil }
        return CGFloat(d.double(forKey: key))
    }

    static func save(_ w: CGFloat) {
        UserDefaults.standard.set(Double(w), forKey: key)
    }

    static func clamp(_ w: CGFloat) -> CGFloat {
        min(Overlay.maxWidth, max(Overlay.minWidth, w))
    }
}

/// Full-pill drag handle. Does not set cursor (non-activating panel; foreground resets it);
/// overlay keeps arrow vs I-beam from SwiftUI text.
private final class DragView: NSView {
    /// Move: begin; end with new top-left.
    var onDragBegan: (() -> Void)?
    var onDragEnded: ((NSPoint) -> Void)?
    /// Resize: begin; step(width, leftEdge); end. Left edge keeps right fixed.
    var onResizeBegan: (() -> Void)?
    var onResize: ((CGFloat, Bool) -> Void)?
    var onResizeEnded: (() -> Void)?

    private enum Mode { case move, resizeLeft, resizeRight }
    private var mode: Mode = .move
    private var initialMouse: NSPoint = .zero
    private var initialOrigin: NSPoint = .zero
    private var initialWidth: CGFloat = 0

    // First click in inactive window must start drag (else swallowed as activation).
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        guard let win = window else { return }
        let p = convert(event.locationInWindow, from: nil)
        if p.x <= Overlay.edgeMargin {
            mode = .resizeLeft
        } else if p.x >= bounds.width - Overlay.edgeMargin {
            mode = .resizeRight
        } else {
            mode = .move
        }
        initialMouse = NSEvent.mouseLocation
        initialOrigin = win.frame.origin
        initialWidth = win.frame.width
        if mode == .move {
            onDragBegan?()
        } else {
            // Show resize cursor for the drag (hover is unreliable from non-activating).
            NSCursor.resizeLeftRight.push()
            onResizeBegan?()
        }
    }

    override func mouseDragged(with event: NSEvent) {
        guard let win = window else { return }
        let now = NSEvent.mouseLocation
        let dx = now.x - initialMouse.x
        switch mode {
        case .move:
            win.setFrameOrigin(
                NSPoint(
                    x: (initialOrigin.x + dx).rounded(),
                    y: (initialOrigin.y + now.y - initialMouse.y).rounded()
                ))
        case .resizeRight:
            onResize?(OverlayWidth.clamp(initialWidth + dx), false)
        case .resizeLeft:
            onResize?(OverlayWidth.clamp(initialWidth - dx), true)
        }
    }

    override func mouseUp(with event: NSEvent) {
        guard let win = window else { return }
        switch mode {
        case .move:
            onDragEnded?(NSPoint(x: win.frame.minX, y: win.frame.maxY))
        case .resizeLeft, .resizeRight:
            NSCursor.pop()
            onResizeEnded?()
        }
    }
}

/// Singleton panel owner; Core hands off dictation snapshots without holding UI.
@MainActor
final class DictationPanelController {
    static let shared = DictationPanelController()

    private let model = DictationModel()
    private let panel: OverlayPanel
    private let hosting: NSHostingView<DictationOverlay>
    private let dragView = DragView()
    /// Suppress apply(...) reposition mid-drag.
    private var isDragging = false
    private var width = Overlay.width

    private init() {
        width = OverlayWidth.saved.map(OverlayWidth.clamp) ?? Overlay.width
        model.width = width
        hosting = NSHostingView(rootView: DictationOverlay(model: model))
        panel = OverlayPanel(
            contentRect: NSRect(x: 0, y: 0, width: width, height: 96),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true
        )
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        // Mouse for drag; stays non-key so paste target keeps focus.
        panel.ignoresMouseEvents = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]

        let container = NSView(frame: NSRect(x: 0, y: 0, width: width, height: 96))
        hosting.frame = container.bounds
        hosting.autoresizingMask = [.width, .height]
        dragView.frame = container.bounds
        dragView.autoresizingMask = [.width, .height]
        container.addSubview(hosting)
        container.addSubview(dragView)
        panel.contentView = container

        dragView.onDragBegan = { [weak self] in self?.isDragging = true }
        dragView.onDragEnded = { [weak self] topLeft in
            self?.isDragging = false
            OverlayPosition.save(topLeft)
        }
        // Edge resize: live width; left edge also updates persisted top-left.
        dragView.onResizeBegan = { [weak self] in self?.isDragging = true }
        dragView.onResize = { [weak self] newWidth, leftEdge in
            self?.applyResize(newWidth, leftEdge: leftEdge)
        }
        dragView.onResizeEnded = { [weak self] in
            guard let self else { return }
            self.isDragging = false
            OverlayWidth.save(self.width)
            OverlayPosition.save(NSPoint(x: self.panel.frame.minX, y: self.panel.frame.maxY))
        }
    }

    /// Live edge resize: opposite edge + top stay put (grows down).
    private func applyResize(_ newWidth: CGFloat, leftEdge: Bool) {
        let old = panel.frame
        width = newWidth
        model.width = newWidth
        hosting.layoutSubtreeIfNeeded()
        let h = max(Overlay.restHeight, hosting.fittingSize.height)
        let topY = old.maxY
        let minX = leftEdge ? (old.maxX - newWidth) : old.minX
        panel.setFrame(
            NSRect(
                x: minX.rounded(), y: (topY - h).rounded(),
                width: newWidth, height: h),
            display: true)
    }

    /// Apply Core dictation snapshot; show gate is `dictation.state` token.
    func apply(
        state: String,
        recording: Bool, awaiting: Bool, text: String, target: String?, local: Bool, hasTarget: Bool,
        promptGlow: Bool, refused: Bool
    ) {
        // != guards (same as Core.apply) — @Observable has no equality short-circuit.
        let target = (target?.isEmpty == false) ? target : nil
        // Refused start reuses no-target orange wash (single overlay cue).
        let hasTarget = hasTarget && !refused
        if model.recording != recording { model.recording = recording }
        if model.awaiting != awaiting { model.awaiting = awaiting }
        if model.text != text { model.text = text }
        if model.target != target { model.target = target }
        if model.hasTarget != hasTarget { model.hasTarget = hasTarget }
        if model.promptGlow != promptGlow { model.promptGlow = promptGlow }

        // Canonical state token (ds-status dictation_state) — same show gate every platform.
        let show: Bool
        switch state {
        case "hidden": show = false
        case "recording", "awaiting_confirm", "refused": show = true
        // Older engine: legacy boolean derivation.
        default: show = awaiting || (recording && local) || refused
        }
        guard show else {
            if panel.isVisible { panel.orderOut(nil) }
            return
        }
        resizeAndPosition()
        // orderFrontRegardless — not makeKey — keeps accessory non-activating.
        panel.orderFrontRegardless()
    }

    /// Fit height + place at remembered top-left (or default). Skip mid-drag.
    private func resizeAndPosition() {
        guard !isDragging else { return }

        hosting.layoutSubtreeIfNeeded()
        let fit = hosting.fittingSize
        let h = max(Overlay.restHeight, fit.height)
        panel.setContentSize(NSSize(width: width, height: h))

        let topLeft: NSPoint
        let screen: NSScreen?
        if let saved = OverlayPosition.saved {
            topLeft = saved
            screen = NSScreen.screens.first { $0.frame.contains(saved) } ?? NSScreen.main
        } else {
            screen = NSScreen.main ?? NSScreen.screens.first
            guard let vf = screen?.visibleFrame else { return }
            // Default lower-center (~22% up + resting height).
            topLeft = NSPoint(
                x: vf.midX - width / 2,
                y: vf.minY + vf.height * 0.22 + Overlay.restHeight)
        }
        guard let vf = screen?.visibleFrame else { return }

        // Top-left → bottom-left origin; clamp so disconnected displays can't strand.
        let x = min(max(topLeft.x, vf.minX), vf.maxX - width)
        let y = min(max(topLeft.y - h, vf.minY), vf.maxY - h)
        panel.setFrameOrigin(NSPoint(x: x.rounded(), y: y.rounded()))
    }
}

/// Overlay content: status + transcript (+ confirm hint).
struct DictationOverlay: View {
    var model: DictationModel
    /// One flip under repeatForever keeps glow pulsing without a timer.
    @State private var breathe = false

    /// Engine `prompt_glow` (shared with Windows).
    private var prompting: Bool { model.promptGlow }

    /// Whole-card orange wash when no paste target (separate from white speak-now ring).
    private var noTarget: Bool { !model.hasTarget }

    var body: some View {
        // Per-word Text (position·text id) so only changed tail blurReplace; prefix holds.
        ZStack(alignment: .topLeading) {
            // Hidden one-line Text reserves height so first word doesn't jump the glass.
            Text(" ")
                .font(Overlay.font)
                .hidden()
            FlowLayout(spacing: 5, lineSpacing: 3) {
                ForEach(words, id: \.id) { w in
                    Text(w.text)
                        .font(Overlay.font)
                        .foregroundStyle(model.text.isEmpty ? .secondary : .primary)
                        .transition(Self.wordTransition)
                }
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .animation(.easeInOut(duration: 0.22), value: displayText)
        .padding(Overlay.pad)
        .frame(width: model.width, alignment: .leading)
        .glassBackground()
        // No-target: shared smWarning wash (separate layer from white speak-now ring).
        .overlay {
            RoundedRectangle(cornerRadius: Overlay.corner, style: .continuous)
                .fill(Color.smWarning)
                .opacity(noTarget ? (breathe ? 0.28 : 0.14) : 0)
                .animation(.easeInOut(duration: 1.2).repeatForever(autoreverses: true), value: breathe)
                .animation(.easeInOut(duration: 0.3), value: noTarget)
                .allowsHitTesting(false)
        }
        .overlay {
            RoundedRectangle(cornerRadius: Overlay.corner, style: .continuous)
                .strokeBorder(.white.opacity(0.6), lineWidth: 1.5)
                .blur(radius: breathe ? 5 : 1.5)
                .shadow(color: .white.opacity(0.5), radius: breathe ? 16 : 5)
                .opacity(prompting ? (breathe ? 0.7 : 0.18) : 0)
                .animation(.easeInOut(duration: 1.2).repeatForever(autoreverses: true), value: breathe)
                .animation(.easeOut(duration: 0.3), value: prompting)
                .allowsHitTesting(false)
        }
        // No outer shadow — window is card-sized; outward glow clips to a dark rect.
        .onAppear { breathe = true }
    }

    private var displayText: String { model.text }

    private static var wordTransition: AnyTransition { AnyTransition(.blurReplace) }

    /// Words with stable `position·word` ids for per-word transitions.
    private var words: [(id: String, text: String)] {
        displayText.split(separator: " ", omittingEmptySubsequences: true)
            .enumerated()
            .map { (i, w) in (id: "\(i)\u{00B7}\(w)", text: String(w)) }
    }
}

/// Wrapping flow so each word is its own view (per-word transitions).
private struct FlowLayout: Layout {
    var spacing: CGFloat = 5
    var lineSpacing: CGFloat = 3

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxW = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineH: CGFloat = 0
        var widest: CGFloat = 0
        for v in subviews {
            let s = v.sizeThatFits(.unspecified)
            if x > 0 && x + s.width > maxW {
                x = 0
                y += lineH + lineSpacing
                lineH = 0
            }
            x += s.width + spacing
            lineH = max(lineH, s.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: min(maxW, widest), height: y + lineH)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let maxW = bounds.width
        var x: CGFloat = bounds.minX
        var y: CGFloat = bounds.minY
        var lineH: CGFloat = 0
        for v in subviews {
            let s = v.sizeThatFits(.unspecified)
            if x > bounds.minX && (x - bounds.minX) + s.width > maxW {
                x = bounds.minX
                y += lineH + lineSpacing
                lineH = 0
            }
            v.place(at: CGPoint(x: x, y: y), anchor: .topLeading, proposal: ProposedViewSize(s))
            x += s.width + spacing
            lineH = max(lineH, s.height)
        }
    }
}
