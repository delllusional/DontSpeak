// Window chrome helpers: close-only, clear glass host, locked sidebar, accessory reopen.

import AppKit
import SwiftUI

/// Reach the hosting `NSWindow` from SwiftUI. Zero-size view resolves `window` once attached.
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

    /// Configure exactly once when `window` exists. Re-running would re-clamp a user-dragged
    /// frame. Reopened windows rebuild the tree → fresh coordinator → configure again.
    private func apply(_ view: NSView, _ coordinator: Coordinator) {
        guard !coordinator.configured else { return }
        // Task@MainActor avoids @Sendable friction with non-Sendable AppKit view/configure.
        Task { @MainActor in
            guard !coordinator.configured, let window = view.window else { return }
            coordinator.configured = true
            configure(window)
        }
    }

    final class Coordinator { var configured = false }
}

extension View {
    /// Close-only chrome (no min/zoom). One-shot — no observer fighting AppKit on resize.
    func closeOnlyWindow() -> some View {
        background(
            WindowAccessor { window in
                window.styleMask.remove(.miniaturizable)
                window.collectionBehavior.insert(.fullScreenNone)
                window.standardWindowButton(.zoomButton)?.isEnabled = false
                window.standardWindowButton(.miniaturizeButton)?.isEnabled = false
                // Skip frame restoration so every open uses compact defaultSize, not last drag.
                // Cocoa restorable + empty autosave name = macOS-14 stand-in for
                // `.restorationBehavior(.disabled)` (15+).
                window.isRestorable = false
                window.setFrameAutosaveName("")
            })
    }
}

extension View {
    /// Transparent host so only SwiftUI `windowGlass()` shows (same pattern as DictationPanel).
    func glassWindow() -> some View {
        background(
            WindowAccessor { window in
                window.isOpaque = false
                window.backgroundColor = .clear
            })
    }
}

@MainActor private func firstSplitView(in view: NSView) -> NSSplitView? {
    if let split = view as? NSSplitView { return split }
    for sub in view.subviews {
        if let found = firstSplitView(in: sub) { return found }
    }
    return nil
}

extension View {
    /// Pin sidebar width and forbid collapse. Leave the split-view delegate alone —
    /// NavigationSplitView's NSSplitViewController is its own delegate; wrapping it left
    /// AppKit calling a freed proxy → segfault. Set item thickness via the controller instead;
    /// if a future SwiftUI hierarchy change misses us, this no-ops (draggable again), never crashes.
    func lockSidebarDivider(width: CGFloat = 150) -> some View {
        background(
            WindowAccessor { window in
                guard let content = window.contentView,
                    let split = firstSplitView(in: content),
                    let controller = split.delegate as? NSSplitViewController,
                    let sidebar = controller.splitViewItems.first
                else { return }
                sidebar.canCollapse = false
                sidebar.minimumThickness = width
                sidebar.maximumThickness = width
            })
    }
}

extension OpenWindowAction {
    /// Activate accessory app first, or the new window opens behind the frontmost app.
    func activating(_ id: String) {
        NSApp.activate()  // `activate(ignoringOtherApps:)` deprecated as of macOS 14
        self(id: id)
    }
}

/// Bridge `openWindow` to AppKit reopen (`applicationShouldHandleReopen` — Finder/Dock/etc.).
/// Accessory apps have no auto-reveal window; both tray Settings and reopen land on `main`.
@MainActor final class WindowOpener {
    static let shared = WindowOpener()
    private var open: OpenWindowAction?

    /// Keep the first registration only — menu-bar label re-runs every animation frame.
    func register(_ action: OpenWindowAction) {
        if open == nil { open = action }
    }

    func openMain() { open?.activating("main") }
}
