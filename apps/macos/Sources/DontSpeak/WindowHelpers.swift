//  WindowHelpers.swift
//
//  Shared helpers for the app's auxiliary windows (Status, Tools, About): close-only
//  window chrome and a menu-bar-friendly "open this window" action. Kept here so the
//  three windows share one implementation instead of repeating it.

import SwiftUI
import AppKit

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
        DispatchQueue.main.async {
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

extension OpenWindowAction {
    /// Bring the (accessory) app forward, then open one of its windows. Accessory apps
    /// aren't frontmost, so a window opened from the menu bar would otherwise appear
    /// behind whatever app currently is.
    func activating(_ id: String) {
        NSApp.activate(ignoringOtherApps: true)
        self(id: id)
    }
}
