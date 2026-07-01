//  DictationPanel.swift
//
//  The dictation confirm overlay — a small, translucent, NON-ACTIVATING floating
//  panel that appears while you dictate. It shows the running transcript live
//  (the engine streams partials into `model_status`), then, once you release
//  Caps Lock, the finalized transcript awaiting your confirm tap.
//
//  Confirm/cancel happen entirely on the Caps Lock key IN THE ENGINE (a quick tap
//  pastes, a long-press discards) — so this panel has NO buttons and never takes
//  keyboard focus: it MUST stay non-activating so the paste lands in the app you
//  were dictating into (e.g. the Claude terminal), not in this overlay.
//
//  Driven purely by `Core`'s pushed `dictation` state via `apply(...)`; it owns no
//  control surface of its own.

import AppKit
import SwiftUI

/// One source of truth for the overlay's geometry + type, shared by the panel
/// controller (AppKit sizing) and the SwiftUI content, so the two can't drift.
private enum Overlay {
    /// Default width; the user's chosen width is persisted (see `OverlayWidth`) and can
    /// be changed by dragging the pill's left/right edges.
    static let width: CGFloat = 460
    /// Horizontal-resize bounds + the grab margin (px from each side edge) within which
    /// a drag resizes instead of moves.
    static let minWidth: CGFloat = 280
    static let maxWidth: CGFloat = 900
    static let edgeMargin: CGFloat = 8
    static let corner: CGFloat = 18
    static let pad: CGFloat = 14
    static let fontSize: CGFloat = 16
    static let font: Font = .system(size: fontSize, weight: .medium)
    /// The resting (empty / single-line) pill height: one line of `font` plus the
    /// top+bottom padding. Derived — not a magic number — so it tracks the font and
    /// padding above. The controller anchors the panel's top edge here.
    static var restHeight: CGFloat {
        let f = NSFont.systemFont(ofSize: fontSize, weight: .medium)
        return ceil(f.ascender - f.descender + f.leading) + pad * 2
    }
}

/// Observable mirror of the engine's dictation state, bound to the overlay view.
@Observable @MainActor
final class DictationModel {
    /// The transcript to show: live partial while recording, finalized while confirming.
    var text: String = ""
    /// The app focused when recording started — where the text will be pasted.
    var target: String?
    /// True once the transcript is finalized and waiting for the confirm tap.
    var awaiting: Bool = false
    /// True while audio is being captured (partials still updating).
    var recording: Bool = false
    /// LIVE: is an editable text field focused to receive the paste? When false, the
    /// glow tints to a warning color (regardless of text) — "nowhere to submit this".
    var hasTarget: Bool = true
    /// The engine's "speak now" glow decision (recording, nothing transcribed yet, not
    /// awaiting confirm), computed once in the core (`prompt_glow`) so this overlay and
    /// the Windows one pulse identically — see `DictationOverlay.prompting`.
    var promptGlow: Bool = false
    /// The overlay's current width (user-resizable by dragging the side edges). Drives
    /// the SwiftUI content width so the transcript re-wraps live as the pill resizes.
    var width: CGFloat = Overlay.width
}

/// A borderless, non-activating floating panel: shows over other apps without
/// stealing key focus, so dictation/paste keeps targeting the previous app.
/// It accepts mouse events (so the pill can be dragged) but still never becomes
/// key/main, so the paste lands in the app you were dictating into — not here.
private final class OverlayPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Persisted overlay position: the user-chosen TOP-LEFT point in global screen
/// coords. We store the top-left (not the bottom-left origin) because the pill
/// grows DOWNWARD as the transcript wraps — pinning the top keeps it where the
/// user dropped it regardless of how many lines it ends up showing.
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

/// Persisted overlay WIDTH (the user can drag the left/right edges to resize). Stored
/// separately from the position so width and drop-point are independent.
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

    /// Clamp any candidate width (saved or freshly dragged) to the allowed range.
    static func clamp(_ w: CGFloat) -> CGFloat {
        min(Overlay.maxWidth, max(Overlay.minWidth, w))
    }
}

/// A transparent view layered over the glass that makes the whole pill draggable: it
/// moves the host window with the pointer and reports the resulting top-left on release.
/// The pill has no controls, so capturing every mouse event over it is what we want.
///
/// It deliberately does NOT change the cursor: cursor management from a background app's
/// non-activating panel is unreliable (the foreground app resets it). Sitting on top of
/// the SwiftUI content still keeps the plain arrow over the pill instead of the I-beam.
private final class DragView: NSView {
    /// Move callbacks: begin, and end carrying the new TOP-LEFT point.
    var onDragBegan: (() -> Void)?
    var onDragEnded: ((NSPoint) -> Void)?
    /// Resize callbacks: begin, each step carrying the new (clamped) width and whether
    /// the LEFT edge is being dragged (left → keep the right edge fixed), and end.
    var onResizeBegan: (() -> Void)?
    var onResize: ((CGFloat, Bool) -> Void)?
    var onResizeEnded: (() -> Void)?

    /// A mouse-down within `edgeMargin` of a side resizes; anywhere else moves.
    private enum Mode { case move, resizeLeft, resizeRight }
    private var mode: Mode = .move
    private var initialMouse: NSPoint = .zero
    private var initialOrigin: NSPoint = .zero
    private var initialWidth: CGFloat = 0

    // The app is an accessory and rarely active, so act on the first click in an
    // inactive window — otherwise the first drag would be swallowed as activation.
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
            // The resize affordance the system can't give us from a non-activating
            // panel on hover: at least show it for the duration of the drag.
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
            // Report the top-left (origin is bottom-left; top is maxY).
            onDragEnded?(NSPoint(x: win.frame.minX, y: win.frame.maxY))
        case .resizeLeft, .resizeRight:
            NSCursor.pop()
            onResizeEnded?()
        }
    }
}

/// Owns the single dictation overlay panel and shows/hides it from `Core`'s status push.
/// A `@MainActor` singleton so the read-only `Core` bridge can hand off the
/// dictation snapshot with one call without holding a UI reference itself.
@MainActor
final class DictationPanelController {
    static let shared = DictationPanelController()

    private let model = DictationModel()
    private let panel: OverlayPanel
    private let hosting: NSHostingView<DictationOverlay>
    /// Transparent drag handle layered over the glass (see `DragView`).
    private let dragView = DragView()
    /// True while the user is dragging the pill, so the `apply(...)` push doesn't
    /// reposition it out from under the pointer mid-drag.
    private var isDragging = false
    /// Overlay width — the user's persisted choice (edge-drag resizable); height tracks
    /// the transcript and grows downward.
    private var width = Overlay.width

    private init() {
        // Restore the user's chosen width (clamped) before sizing anything.
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
        // Accept mouse events so the pill can be dragged. The panel stays
        // non-activating (canBecomeKey == false), so dragging never steals keyboard
        // focus and the paste still lands in the app being dictated into.
        panel.ignoresMouseEvents = false
        // Float across spaces and over fullscreen apps; don't join window cycling.
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]

        // Container holding the SwiftUI glass with the drag handle on top; both
        // auto-resize with the panel as the transcript grows.
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
        // Edge-drag horizontal resize: pause repositioning during the drag, apply each
        // step live, then persist the new width AND top-left (a left-edge resize moves
        // the left side, so the remembered drop point changes too).
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

    /// Live horizontal resize from an edge drag: set the new width, re-wrap the
    /// transcript (recomputing height), and reframe so the OPPOSITE edge and the TOP
    /// stay put — the pill grows downward, so the top is the vertical anchor, and the
    /// edge NOT being dragged is the horizontal anchor.
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

    /// Apply one dictation snapshot from `Core`. Shows the panel while recording or
    /// awaiting confirmation, hides it otherwise. Cheap to call on every push.
    func apply(
        recording: Bool, awaiting: Bool, text: String, target: String?, local: Bool, hasTarget: Bool,
        promptGlow: Bool
    ) {
        model.recording = recording
        model.awaiting = awaiting
        model.text = text
        model.target = (target?.isEmpty == false) ? target : nil
        model.hasTarget = hasTarget
        model.promptGlow = promptGlow

        // Show as soon as a LOCAL (Parakeet) dictation starts recording — immediate
        // feedback ("Listening…") the moment you tap Caps, not after the first
        // partial — or when a transcript is awaiting confirmation. The `local` gate
        // keeps the overlay scoped to the local-transcript path: ClaudeNative
        // produces no partials and submits straight to Claude, so it never shows
        // (recording is true but `local` is false).
        let show = awaiting || (recording && local)
        guard show else {
            if panel.isVisible { panel.orderOut(nil) }
            return
        }
        resizeAndPosition()
        // orderFrontRegardless (NOT makeKey…) shows it without activating this
        // accessory app, so the dictation target keeps focus.
        panel.orderFrontRegardless()
    }

    /// Size to the SwiftUI content's fitting height and place the panel. Uses the
    /// user's remembered drop point if they've dragged it before, else the default
    /// lower-center of the main screen. Skipped while a drag is in progress so the
    /// push can't yank the pill out from under the pointer.
    private func resizeAndPosition() {
        guard !isDragging else { return }

        hosting.layoutSubtreeIfNeeded()
        let fit = hosting.fittingSize
        let h = max(Overlay.restHeight, fit.height)
        panel.setContentSize(NSSize(width: width, height: h))

        // Resolve the TOP-LEFT to pin to (the panel grows downward from it), plus
        // the screen to clamp against.
        let topLeft: NSPoint
        let screen: NSScreen?
        if let saved = OverlayPosition.saved {
            topLeft = saved
            screen = NSScreen.screens.first { $0.frame.contains(saved) } ?? NSScreen.main
        } else {
            screen = NSScreen.main ?? NSScreen.screens.first
            guard let vf = screen?.visibleFrame else { return }
            // Default: lower-center, top edge at the ~22%-up baseline + resting height.
            topLeft = NSPoint(
                x: vf.midX - width / 2,
                y: vf.minY + vf.height * 0.22 + Overlay.restHeight)
        }
        guard let vf = screen?.visibleFrame else { return }

        // Top-left → bottom-left origin, then clamp fully on-screen so a remembered
        // point near an edge (or on a now-disconnected display) can't strand the pill.
        let x = min(max(topLeft.x, vf.minX), vf.maxX - width)
        let y = min(max(topLeft.y - h, vf.minY), vf.maxY - h)
        panel.setFrameOrigin(NSPoint(x: x.rounded(), y: y.rounded()))
    }
}

/// The overlay's content: a status line (state + paste target), the transcript,
/// and — while confirming — a faint hint of the Caps gesture. Translucent (Liquid
/// Glass where available, else an ultra-thin material) to stay light on the eye.
struct DictationOverlay: View {
    var model: DictationModel
    /// Drives the breathing glow. Flipped true once and left there: a single
    /// state change under a `repeatForever(autoreverses:)` animation oscillates
    /// perpetually, so the glow keeps pulsing without a timer.
    @State private var breathe = false

    /// Recording but nothing recognized yet (and not the finalized-empty case): the panel
    /// is empty and waiting, so glow to prompt the user to speak. Decided ONCE in the
    /// engine (`prompt_glow`) and shared with the Windows overlay so the two can't drift.
    private var prompting: Bool { model.promptGlow }

    /// No editable field is focused to receive the paste. Warns the user — regardless of
    /// whether there's transcript text yet — by glowing the WHOLE card orange (a separate
    /// layer from the white speak-now ring, so the two never collide).
    private var noTarget: Bool { !model.hasTarget }

    var body: some View {
        // The transcript, ONE Text per word in a wrapping flow. Each word is keyed by
        // position + text, so as a streaming partial grows only the changed words (the new
        // tail, or a refined last word) blur in/out via `.blurReplace` — the stable prefix
        // stays put instead of the whole line re-blurring. The flow's position animation
        // slides existing words as the wrap shifts. SF Pro .medium per the Typography HIG.
        ZStack(alignment: .topLeading) {
            // Reserve exactly one line's height (a hidden, same-font Text) so the glass
            // is already one-line tall while empty — the panel no longer grows/jumps
            // when the first word appears. Empty state == first-line state.
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
        // No-target cue: the WHOLE card glows by washing the glass with the shared
        // WARNING orange (`Color.smWarning` / `Brand.warning`) — the SAME color as the
        // warming/reloading status dot, from the one cross-platform source of truth.
        // Pulses with the breath. A SEPARATE layer from the white speak-now ring below,
        // so they never collide.
        .overlay {
            RoundedRectangle(cornerRadius: Overlay.corner, style: .continuous)
                .fill(Color.smWarning)
                .opacity(noTarget ? (breathe ? 0.28 : 0.14) : 0)
                .animation(.easeInOut(duration: 1.2).repeatForever(autoreverses: true), value: breathe)
                .animation(.easeInOut(duration: 0.3), value: noTarget)
                .allowsHitTesting(false)
        }
        // Breathing WHITE "speak now" ring while waiting for the first word.
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
        // NOTE: no OUTER shadow/halo here — the panel window is sized exactly to the
        // card, so an outward glow would be clipped to the window's square bounds and
        // read as a dark rectangle around the card. The in-bounds orange wash above is
        // the whole-card glow; the pulse gives it life.
        .onAppear { breathe = true }
    }

    /// The transcript, or empty — the panel stays bare (just the glass) when nothing was
    /// recognized, including the finalized-but-empty case (no placeholder text).
    private var displayText: String { model.text }

    /// Per-word transition: the soft "Liquid Glass" blur-replace (macOS 14+, the app's
    /// floor).
    private static var wordTransition: AnyTransition { AnyTransition(.blurReplace) }

    /// The transcript split into per-word tokens, each with a stable id of
    /// `position·word`. A word keeps its id (no transition) while it and its
    /// position are unchanged; an appended or refined word gets a new id, so only
    /// that word blur-replaces. Empty transcript → no words (clear glass).
    private var words: [(id: String, text: String)] {
        displayText.split(separator: " ", omittingEmptySubsequences: true)
            .enumerated()
            .map { (i, w) in (id: "\(i)\u{00B7}\(w)", text: String(w)) }
    }
}

/// Minimal wrapping flow layout: lays subviews left-to-right, wrapping to a new row
/// when the next subview would overflow the proposed width. Used so each transcript
/// word is its own view (for per-word transitions) while still reading as flowing text.
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
