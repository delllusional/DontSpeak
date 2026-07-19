// @main: accessory MenuBarExtra + sidebar window. Hosts the engine in-process
// (`ds_engine_start` / `ds_engine_stop`).

import AppKit
import CDontSpeak
import ServiceManagement
import SwiftUI

@main
struct DontSpeakApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var core = Core()

    var body: some Scene {
        MenuBarExtra {
            TrayMenu()
                .environment(core)
        } label: {
            MenuBarLabel()
                .environment(core)
        }
        .menuBarExtraStyle(.menu)

        // Empty title + hidden title bar: frosted state-tint strip reads as the chrome.
        Window("", id: "main") {
            MainWindow()
                .environment(core)
        }
        .windowResizability(.contentMinSize)
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 510, height: 320)
    }

}

/// Menu-bar label: idle template / orange recording / purple speaking via `TrayAnimator`.
/// Same `TrayState` source as the window title-bar indicator.
private struct MenuBarLabel: View {
    @Environment(Core.self) private var core
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        // Side effect via `let _ =` — NOT a view modifier — so the returned view stays a bare
        // Image and the status item hugs the glyph. Label is the only view at launch, so reopen
        // is ready before the first Finder/Dock click.
        let _ = WindowOpener.shared.register(openWindow)
        // MUST be bare `Image(nsImage:)`: any modifier balloons the status item to dropdown width.
        // Animation + mute slash are baked into the NSImage by TrayAnimator.
        Image(nsImage: core.trayAnimator.image)
    }
}

/// Live tray state shared by menu-bar + title-bar indicator. Glyph is always the brand icon;
/// only the pill behind it changes. Recording wins over speaking when both apply (full-duplex
/// live-mic cue must override purple).
enum TrayState: Equatable {
    case idle, recording, speaking

    /// True only for `_animated` tray_indicator forms. Drives TrayAnimator breathing
    /// (macOS-only; Windows tints without breathing). Independent of shared kind resolution.
    @MainActor static func animated(_ core: Core) -> Bool {
        let cfg = core.activity.trayIndicator
        switch current(core) {
        case .recording: return cfg.contains("stt_animated")
        case .speaking: return cfg.contains("tts_animated")
        case .idle: return false
        }
    }

    /// `tray_indicator` gates color ([] = never). Shared `ds_tray_icon_kind` — one rule with Win/Linux.
    @MainActor static func current(_ core: Core) -> TrayState {
        switch Core.trayIconKind(
            sttActive: core.activity.recording,
            ttsActive: core.activity.speaking,
            trayIndicator: core.activity.trayIndicator
        ) {
        case "recording": return .recording
        case "speaking": return .speaking
        default: return .idle
        }
    }

    /// Pill tint; nil when idle. Single color source for menu-bar + title-bar.
    @MainActor var tint: NSColor? {
        switch self {
        case .idle: return nil
        case .recording: return Brand.micOrange
        case .speaking: return Brand.seedPurple
        }
    }

    @MainActor var image: NSImage {
        switch self {
        case .idle: return Self.brandIcon
        case .recording: return Self.recordingPill
        case .speaking: return Self.speakingPill
        }
    }

    /// Mute slash baked into NSImage (not a SwiftUI overlay) so the label stays bare.
    @MainActor func image(muted: Bool) -> NSImage {
        let base = image
        return muted ? Self.applySlash(to: base, tint: self.tint) : base
    }

    /// Diagonal mute slash. `tint == nil` → transparent knockout on idle template; else pill gap + white slash.
    @MainActor static func applySlash(to base: NSImage, tint: NSColor?) -> NSImage {
        let out = NSImage(size: base.size, flipped: false) { rect in
            base.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1)
            let h = rect.height
            let d = h * 0.30
            let slash = NSBezierPath()
            slash.move(to: NSPoint(x: rect.midX - d, y: rect.midY - d))
            slash.line(to: NSPoint(x: rect.midX + d, y: rect.midY + d))
            slash.lineCapStyle = .round
            guard let ctx = NSGraphicsContext.current else { return true }
            if let tint {
                slash.lineWidth = h * 0.20
                tint.setStroke()
                slash.stroke()
                slash.lineWidth = h * 0.09
                NSColor.white.setStroke()
                slash.stroke()
            } else {
                slash.lineWidth = h * 0.20
                ctx.compositingOperation = .destinationOut
                NSColor.black.setStroke()
                slash.stroke()
                ctx.compositingOperation = .sourceOver
                slash.lineWidth = h * 0.09
                NSColor.black.setStroke()
                slash.stroke()
            }
            return true
        }
        out.isTemplate = base.isTemplate
        return out
    }

    // @MainActor caches: NSImage isn't Sendable; colors come from `tint`.
    @MainActor static let brandIcon = MenuBarIcon.icon(tint: TrayState.idle.tint)
    @MainActor static let recordingPill = MenuBarIcon.icon(tint: TrayState.recording.tint)
    @MainActor static let speakingPill = MenuBarIcon.icon(tint: TrayState.speaking.tint)

    /// Only the pill at `pillAlpha`; white glyph stays opaque (TrayAnimator breathing).
    @MainActor func breathingImage(muted: Bool, pillAlpha: CGFloat) -> NSImage {
        guard let tint = self.tint else { return image(muted: muted) }
        let base = MenuBarIcon.icon(tint: tint, pillAlpha: pillAlpha)
        return muted ? Self.applySlash(to: base, tint: tint) : base
    }

    /// Crossfade-safe: idle template would blend as raw black — resolve live labelColor into a
    /// concrete non-template frame. Settled idle still uses the real template (`image`).
    @MainActor func crossfadeImage(muted: Bool) -> NSImage {
        guard self.tint == nil else { return image(muted: muted) }
        // Force colorspace conversion under the menu-bar appearance so the dynamic catalog
        // color pins light/dark for this blend frame (assigning alone leaves it dynamic).
        var color = NSColor.labelColor
        NSApp.effectiveAppearance.performAsCurrentDrawingAppearance {
            color = NSColor.labelColor.usingColorSpace(.sRGB) ?? .labelColor
        }
        let g = MenuBarIcon.tintedGlyph(color)
        return muted ? Self.applySlash(to: g, tint: color) : g
    }
}

/// Brand glyph: SVG preferred (crisp at any scale), then PNG, then SF Symbol.
private func brandGlyph(height: CGFloat) -> NSImage {
    if let url = Bundle.main.url(forResource: "MenuBarIcon", withExtension: "svg")
        ?? Bundle.main.url(forResource: "MenuBarIcon", withExtension: "png"),
        let img = NSImage(contentsOf: url)
    {
        img.size = NSSize(width: height, height: height)
        return img
    }
    let cfg = NSImage.SymbolConfiguration(pointSize: height, weight: .bold)
    return NSImage(systemSymbolName: "waveform.circle.fill", accessibilityDescription: nil)?
        .withSymbolConfiguration(cfg) ?? NSImage()
}

/// Cached menu-bar geometry. Pill fills bar height like the system mic indicator (aspect 40:24);
/// every state shares one footprint so switches never shift the item.
/// `NSStatusBar.system.thickness` under-reports 22 on many Macs (FB8503857) — max with 24.
@MainActor
private enum MenuBarIcon {
    static let micPillAspect: CGFloat = 40.0 / 24.0
    static let glyphFill: CGFloat = 0.88
    static let h = max(NSStatusBar.system.thickness, 24)
    static let w = (h * micPillAspect).rounded()
    static let size = NSSize(width: w, height: h)
    static let glyph = brandGlyph(height: (h * glyphFill).rounded())
    static let gx = (w - glyph.size.width) / 2
    static let gy = (h - glyph.size.height) / 2
    static let whiteGlyph = NSImage(size: glyph.size, flipped: false) { r in
        glyph.draw(in: r)
        NSColor.white.set()
        r.fill(using: .sourceAtop)
        return true
    }

    /// Non-template glyph at idle geometry — crossfade-safe idle (template blends black).
    static func tintedGlyph(_ color: NSColor) -> NSImage {
        let g = NSImage(size: glyph.size, flipped: false) { r in
            glyph.draw(in: r)
            color.set()
            r.fill(using: .sourceAtop)
            return true
        }
        let img = NSImage(size: size, flipped: false) { _ in
            g.draw(at: NSPoint(x: gx, y: gy), from: .zero, operation: .sourceOver, fraction: 1)
            return true
        }
        img.isTemplate = false
        return img
    }

    /// `tint == nil` → idle template; else white glyph on capsule at `pillAlpha` (breathing fades only the pill).
    static func icon(tint: NSColor?, pillAlpha: CGFloat = 1) -> NSImage {
        let img = NSImage(size: size, flipped: false) { rect in
            if let tint {
                tint.withAlphaComponent(pillAlpha).setFill()
                NSBezierPath(roundedRect: rect, xRadius: h / 2, yRadius: h / 2).fill()
                whiteGlyph.draw(at: NSPoint(x: gx, y: gy), from: .zero, operation: .sourceOver, fraction: 1)
            } else {
                glyph.draw(at: NSPoint(x: gx, y: gy), from: .zero, operation: .sourceOver, fraction: 1)
            }
            return true
        }
        img.isTemplate = (tint == nil)
        return img
    }
}

/// Accessory policy, login item, bundled dylib env, engine start/stop.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        registerLoginItem()
        useBundledOnnxRuntimeIfPresent()
        useBundledKokoroCoreMLIfPresent()
        useBundledSeparatorIfPresent()
        // In-process: caps loop + RPC + TTS on a bg thread. Accessibility / Input-Monitoring /
        // Mic all grant to this one signed bundle. MCP/hooks hit the socket we serve.
        _ = ds_engine_start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        _ = ds_engine_stop()
    }

    /// Finder/Launchpad/Dock reopen: accessory has no auto-reveal — open `main` like Docker/Dropbox.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        WindowOpener.shared.openMain()
        return true
    }

    /// Notarized builds ship ORT in Frameworks (no runtime download / Gatekeeper block).
    /// Set before engine start; helper inherits. Local builds no-op → engine downloads on first use.
    private func useBundledOnnxRuntimeIfPresent() {
        guard let dylib = Bundle.main.privateFrameworksURL?.appendingPathComponent("libonnxruntime.dylib"),
            FileManager.default.fileExists(atPath: dylib.path)
        else { return }
        setenv("ORT_DYLIB_PATH", dylib.path, 1)
    }

    /// FluidAudio Core ML shim for `tts_provider=apple-native`. Absent (e.g. Intel) → ONNX path.
    private func useBundledKokoroCoreMLIfPresent() {
        guard let dylib = Bundle.main.privateFrameworksURL?.appendingPathComponent("libsmkokoro.dylib"),
            FileManager.default.fileExists(atPath: dylib.path)
        else { return }
        setenv("SMKOKORO_DYLIB_PATH", dylib.path, 1)
    }

    /// Speaker-lock sepformer model. Absent → fail open (unfiltered STT).
    private func useBundledSeparatorIfPresent() {
        guard let model = Bundle.main.resourceURL?.appendingPathComponent("sepformer_int8.onnx"),
            FileManager.default.fileExists(atPath: model.path)
        else { return }
        setenv("DONTSPEAK_SEPARATOR_PATH", model.path, 1)
    }

    /// Login item via SMAppService. Fail-quiet if denied / standalone.
    private func registerLoginItem() {
        let svc = SMAppService.mainApp
        guard svc.status != .enabled else { return }
        do {
            try svc.register()
        } catch {
            NSLog("DontSpeak: login-item registration failed: \(error.localizedDescription)")
        }
    }
}
