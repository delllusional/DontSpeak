//  WindowHelpers.swift
//
//  Shared helpers for the app's auxiliary windows (Status, Tools, About): close-only
//  window chrome and a menu-bar-friendly "open this window" action. Kept here so the
//  three windows share one implementation instead of repeating it.

import SwiftUI
import AppKit
import ObjectiveC

/// Reaches the hosting `NSWindow` from SwiftUI so callers can tweak native window
/// chrome (e.g. disable the minimize/zoom buttons for a close-only window). The
/// zero-size backing view resolves its `window` once it's in the hierarchy.
struct WindowAccessor: NSViewRepresentable {
    let configure: (NSWindow) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        apply(view, context.coordinator)
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        apply(nsView, context.coordinator)
    }

    /// Run `configure` exactly ONCE, as soon as the backing view has a window. The window
    /// may not exist at make time, so both entry points funnel here; the `configured` latch
    /// then keeps a later SwiftUI update pass from re-running it — which for the resizable
    /// window would re-clamp (fight) a frame the user has dragged. A reopened window rebuilds
    /// the view tree → fresh coordinator → it configures again.
    private func apply(_ view: NSView, _ coordinator: Coordinator) {
        guard !coordinator.configured else { return }
        // Defer to the next main-actor turn (the window may not be attached yet at make time).
        // A main-actor `Task` carries the isolation and avoids `@Sendable`-capture friction with
        // the non-Sendable AppKit `view`/`configure`, unlike a bare `DispatchQueue.main.async`.
        Task { @MainActor in
            guard !coordinator.configured, let window = view.window else { return }
            coordinator.configured = true
            configure(window)
        }
    }

    final class Coordinator { var configured = false }
}

extension View {
    /// Close-only chrome: disable the minimize + zoom (green) buttons and bar fullscreen, so
    /// only Close stays live. A one-shot config — the window is resizable with an internal
    /// ScrollView (expanding scrolls, nothing auto-resizes), so the zoom-disable holds with no
    /// observer fighting AppKit re-enabling it on resize (which flickered the green button).
    func closeOnlyWindow() -> some View {
        background(WindowAccessor { window in
            window.styleMask.remove(.miniaturizable)
            window.collectionBehavior.insert(.fullScreenNone)
            window.standardWindowButton(.zoomButton)?.isEnabled = false
            window.standardWindowButton(.miniaturizeButton)?.isEnabled = false
            // Don't restore the last frame. macOS state restoration re-applies the saved
            // "NSWindow Frame main" on every launch, so the window reopened at whatever the user
            // last dragged it to instead of its compact `defaultSize`. Turning off BOTH the Cocoa
            // restorable state and the frame autosave (the macOS-14-compatible equivalent of
            // `.restorationBehavior(.disabled)`, which is macOS 15+) lets the window open at its
            // `idealHeight`/`minHeight` (the snug Status size) every time.
            window.isRestorable = false
            window.setFrameAutosaveName("")
        })
    }
}

extension View {
    /// Make the hosting `NSWindow` itself transparent so a SwiftUI glass slab (see
    /// `windowGlass()`) is the only background — the whole panel reads as one continuous
    /// Liquid-Glass surface with no title strip. Same clear-window setup the dictation
    /// overlay uses (`DictationPanel`: `isOpaque = false` + `backgroundColor = .clear`).
    func glassWindow() -> some View {
        background(WindowAccessor { window in
            window.isOpaque = false
            window.backgroundColor = .clear
        })
    }
}

/// Pins the sidebar/detail divider so it CAN'T be dragged. `navigationSplitViewColumnWidth`
/// fixes the column's preferred width, but the AppKit `NSSplitView` underneath still lets the
/// user grab the divider and drag the panes — far enough to push the sidebar off-screen. The
/// canonical way to disable a divider is a delegate returning a ZERO effective drag rect for it
/// (`splitView(_:effectiveRect:forDrawnRect:ofDividerAt:)`), so there's no hit area to grab.
/// We wrap (not replace) SwiftUI's own split delegate — forwarding every other call to it — so
/// the column layout it manages is untouched; only the drag is removed.
private final class FixedDividerSplitDelegate: NSObject, NSSplitViewDelegate {
    weak var wrapped: NSSplitViewDelegate?
    init(wrapping: NSSplitViewDelegate?) { self.wrapped = wrapping }

    func splitView(_ splitView: NSSplitView, effectiveRect proposedEffectiveRect: NSRect,
                   forDrawnRect drawnRect: NSRect, ofDividerAt dividerIndex: Int) -> NSRect {
        .zero   // no drag hit area → the divider is immovable
    }

    // Transparently forward every OTHER delegate method to SwiftUI's original delegate, so its
    // column sizing/collapse behavior is preserved — we only override the one method above.
    override func responds(to aSelector: Selector!) -> Bool {
        super.responds(to: aSelector) || (wrapped?.responds(to: aSelector) ?? false)
    }
    override func forwardingTarget(for aSelector: Selector!) -> Any? { wrapped }
}

private nonisolated(unsafe) var kFixedDividerDelegateKey: UInt8 = 0

/// Depth-first search for the first `NSSplitView` under a view (the one `NavigationSplitView`
/// builds for the sidebar/detail layout).
@MainActor private func firstSplitView(in view: NSView) -> NSSplitView? {
    if let split = view as? NSSplitView { return split }
    for sub in view.subviews {
        if let found = firstSplitView(in: sub) { return found }
    }
    return nil
}

extension View {
    /// Make the sidebar/detail divider non-draggable (see `FixedDividerSplitDelegate`). Applied
    /// once when the window appears; the wrapping delegate is retained via an associated object
    /// so ARC keeps it alive for the window's lifetime.
    func lockSidebarDivider() -> some View {
        background(WindowAccessor { window in
            guard let content = window.contentView,
                  let split = firstSplitView(in: content) else { return }
            let proxy = FixedDividerSplitDelegate(wrapping: split.delegate)
            objc_setAssociatedObject(split, &kFixedDividerDelegateKey, proxy, .OBJC_ASSOCIATION_RETAIN)
            split.delegate = proxy
        })
    }
}

extension OpenWindowAction {
    /// Bring the (accessory) app forward, then open one of its windows. Accessory apps
    /// aren't frontmost, so a window opened from the menu bar would otherwise appear
    /// behind whatever app currently is.
    func activating(_ id: String) {
        NSApp.activate()   // `activate(ignoringOtherApps:)` is deprecated as of macOS 14
        self(id: id)
    }
}
