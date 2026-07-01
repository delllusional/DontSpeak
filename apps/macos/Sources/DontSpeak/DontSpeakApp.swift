//  DontSpeakApp.swift
//
//  @main entry. A dockless (accessory) MenuBarExtra tray app + one sidebar window
//  (Status / Tools / Logs / Libraries). This app HOSTS the engine in-process and owns its
//  lifecycle: it calls
//  ds_engine_start() on launch and ds_engine_stop() on quit via the C
//  ABI. It also shows status and helps grant OS permissions. All control lives in
//  DontSpeak.

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

        // The single app window: a sidebar of screens (Status / Tools / Logs / Libraries)
        // over one detail pane — the former standalone `status` + `tools` windows merged and
        // the two missing screens added. Empty title + hidden title bar so the window shows no
        // title text (like the system About panel) and the frosted state-tinted strip reads as
        // the title bar. Resizable; each pane scrolls internally.
        Window("", id: "main") {
            MainWindow()
                .environment(core)
        }
        .windowResizability(.contentMinSize)
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 510, height: 320)
    }

}

/// The menu-bar label. It mirrors the engine's live state the way the system mic
/// indicator does (idle template glyph / orange recording pill / purple speaking pill).
/// The state derivation and the glyphs live in the shared `TrayState`, so the status
/// window's title-bar indicator (`TrayStatusIcon`) reflects narration/dictation from the
/// SAME source. Driven by `core` (an `@Observable` in the `@Environment`), so it re-renders
/// as the activity flags it reads flip on each pushed status snapshot.
private struct MenuBarLabel: View {
    @Environment(Core.self) private var core
    var body: some View {
        // The label MUST be a BARE `Image(nsImage:)` with NO view modifiers: in a `.menu`-style
        // MenuBarExtra, wrapping the label in any modifier (`.opacity`, `.fixedSize`, a stack)
        // makes the status-item button stop hugging the image and balloon to the dropdown's
        // width. So all animation is baked INTO the NSImage by `TrayAnimator` (crossfade on
        // state change + breathing while active) and we just render its current frame here —
        // the label stays modifier-free, the mute mark is baked in (`image(muted:)`).
        Image(nsImage: core.trayAnimator.image)
    }
}

/// The live tray state — idle / recording (speech-to-text) / speaking (narration) — and
/// its glyph. Shared by the menu-bar item (`MenuBarLabel`) and the status window's
/// title-bar indicator (`TrayStatusIcon`) so BOTH reflect dictation (orange) and narration
/// (purple) identically, from ONE derivation and ONE set of cached images:
///   • idle      → the brand icon as a tinted template (auto light/dark)
///   • recording → the brand icon (white) on an ORANGE pill — the same orange as the
///     macOS microphone-in-use indicator
///   • speaking  → the brand icon (white) on a purple pill (while the voice talks)
/// The glyph is always DontSpeak's icon — only the pill behind it changes.
enum TrayState: Equatable {
    case idle, recording, speaking

    /// Whether the CURRENT state should BREATHE — true only when its `tray_indicator` token is
    /// the `_animated` form (`stt_animated` / `tts_animated`). A statically-colored state shows
    /// a solid pill; idle never breathes. Drives `TrayAnimator`.
    @MainActor static func animated(_ core: Core) -> Bool {
        let cfg = core.activity.trayIndicator
        switch current(core) {
        case .recording: return cfg.contains("stt_animated")
        case .speaking: return cfg.contains("tts_animated")
        case .idle: return false
        }
    }

    /// Derive the state from the engine's live flags, gated by the `tray_indicator` config
    /// SET (contains "stt" and/or "tts"; [] = never color). RECORDING wins when both apply:
    /// in full-duplex you can dictate WHILE the voice speaks, and the live-mic (orange) cue
    /// must override the speaking (purple) pill so it's obvious the app switched to LISTENING.
    /// `sttActive` is the DICTATION state (a Caps-tap record, NOT the always-on VPIO capture),
    /// so orange shows only while dictating, then falls back to purple if the voice is still
    /// talking. In half-duplex the two never overlap (the mic is gated closed during TTS).
    @MainActor static func current(_ core: Core) -> TrayState {
        let cfg = core.activity.trayIndicator
        if core.activity.sttActive && (cfg.contains("stt") || cfg.contains("stt_animated")) {
            return .recording
        }
        if core.activity.ttsActive && (cfg.contains("tts") || cfg.contains("tts_animated")) {
            return .speaking
        }
        return .idle
    }

    /// The pill tint for this state — `nil` when idle (no colored pill). The SINGLE
    /// definition of the state colors: the menu-bar glyph capsules (below) and the status
    /// window's bare pill (`TrayStatusIcon`) are both built from it, so they can't drift.
    @MainActor var tint: NSColor? {
        switch self {
        case .idle: return nil
        case .recording: return Brand.micOrange
        case .speaking: return Brand.seedPurple
        }
    }

    /// The glyph for this state — the shared cached images below.
    @MainActor var image: NSImage {
        switch self {
        case .idle: return Self.brandIcon
        case .recording: return Self.recordingPill
        case .speaking: return Self.speakingPill
        }
    }

    /// The glyph, optionally with the MUTE mark: a diagonal slash across the icon (like the
    /// system `speaker.slash` / Wi-Fi-unavailable mark) rather than dimming the whole thing.
    /// The slash carries a thin "gap" so it stands off the glyph — pill-colored on the colored
    /// states, a transparent knockout on the idle template glyph. Baked into the NSImage (not a
    /// SwiftUI overlay) so the menu-bar label stays a bare `Image(nsImage:)` that hugs the icon;
    /// `isTemplate` is preserved so the idle glyph still tints to the bar.
    @MainActor func image(muted: Bool) -> NSImage {
        let base = image
        return muted ? Self.applySlash(to: base, tint: self.tint) : base
    }

    /// Bake the mute slash onto `base` — a diagonal slash (like the system `speaker.slash`)
    /// rather than dimming the whole icon. Factored out so `image(muted:)` and `breathingImage`
    /// share it. `tint == nil` ⇒ the idle template path (transparent knockout so the bar tint
    /// shows the gap); else the colored path (pill-colored gap + white slash). `isTemplate` is
    /// preserved so the idle glyph still tints to the bar.
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

    // The three cached state icons. @MainActor: NSImage isn't Sendable and these are only
    // read from the main actor, so isolating the caches keeps them concurrency-safe under
    // Swift 6 without recomputing the glyph on every render. Each is built from its state's
    // `tint`, so the color lives in ONE place (see `tint`).
    @MainActor static let brandIcon = MenuBarIcon.icon(tint: TrayState.idle.tint)
    @MainActor static let recordingPill = MenuBarIcon.icon(tint: TrayState.recording.tint)
    @MainActor static let speakingPill = MenuBarIcon.icon(tint: TrayState.speaking.tint)

    /// The active-state icon with ONLY the pill at `pillAlpha` — the white glyph stays fully
    /// opaque. Used by `TrayAnimator` so just the surrounding capsule breathes while the brand
    /// mark holds steady. Idle has no pill, so it returns the plain glyph unchanged.
    @MainActor func breathingImage(muted: Bool, pillAlpha: CGFloat) -> NSImage {
        guard let tint = self.tint else { return image(muted: muted) }
        let base = MenuBarIcon.icon(tint: tint, pillAlpha: pillAlpha)
        return muted ? Self.applySlash(to: base, tint: tint) : base
    }

    /// A crossfade-SAFE image for blending. Active states are already non-template (white glyph
    /// on a colored pill), so use them as-is. The IDLE glyph is a TEMPLATE the menu bar tints
    /// live — but drawn into a blend bitmap a template renders as its raw BLACK pixels (the
    /// "glyph briefly turns black" flash). So for crossfades, render idle in the live menu-bar
    /// text color (non-template). The SETTLED idle still uses the real template (see `image`),
    /// so it keeps adapting to the bar / appearance.
    @MainActor func crossfadeImage(muted: Bool) -> NSImage {
        guard self.tint == nil else { return image(muted: muted) }
        // RESOLVE the dynamic `labelColor` under the menu bar's effective appearance into a
        // concrete color. `labelColor` is a dynamic catalog color; assigning it inside the
        // appearance block doesn't resolve it (it stays dynamic and would re-resolve under
        // whatever appearance is current at draw time). Forcing a colorspace conversion WHILE the
        // menu-bar appearance is current pins the right light/dark value for the blend frame.
        var color = NSColor.labelColor
        NSApp.effectiveAppearance.performAsCurrentDrawingAppearance {
            color = NSColor.labelColor.usingColorSpace(.sRGB) ?? .labelColor
        }
        let g = MenuBarIcon.tintedGlyph(color)
        return muted ? Self.applySlash(to: g, tint: color) : g
    }
}

/// The brand glyph at a pill-friendly size, as a plain (non-template) image we tint
/// white ourselves. Prefers the VECTOR source (`MenuBarIcon.svg`) so the mark stays
/// crisp at any size — NSImage rasterizes the SVG per device scale on draw — then
/// falls back to the rasterized PNG, then an SF Symbol.
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

// Match the macOS microphone-in-use indicator that sits right next to us: a capsule
// that FILLS the menu-bar height with a large glyph. The pill height tracks the live
// bar thickness (so we fill the bar exactly like the system indicator), the width
// follows the mic pill's measured 40:24 aspect, and the glyph fills ~88% of the bar.
// EVERY state shares this one footprint, so switching never shifts/resizes the item —
// only the colored pill appears/disappears behind a fixed glyph.
private let kMicPillAspect: CGFloat = 40.0 / 24.0  // measured from the system mic pill
private let kGlyphFill: CGFloat = 0.88  // glyph box ÷ bar height (≈ the mic's prominence)

/// Cached menu-bar icon geometry + glyphs, built ONCE (@MainActor; NSImage isn't Sendable
/// and these are main-actor-only). The pill height tracks the bar so we fill it like the
/// system mic indicator; the brand glyph is FIXED across states — only the colored pill
/// behind it changes. Caching the white glyph + geometry lets a breathing frame redraw just
/// the pill and composite the cached glyph, with NO per-frame SVG reload.
@MainActor
private enum MenuBarIcon {
    // The REAL max menu-bar item height is 24 pt (matching the system mic indicator).
    // `NSStatusBar.system.thickness` under-reports as 22 on many Macs — a long-standing Apple
    // bug (FB8503857) — so take the larger of the two.
    static let h = max(NSStatusBar.system.thickness, 24)
    static let w = (h * kMicPillAspect).rounded()
    static let size = NSSize(width: w, height: h)
    static let glyph = brandGlyph(height: (h * kGlyphFill).rounded())
    static let gx = (w - glyph.size.width) / 2
    static let gy = (h - glyph.size.height) / 2
    /// The brand glyph tinted WHITE (for the active, on-pill states); cached so breathing
    /// frames don't re-render it.
    static let whiteGlyph = NSImage(size: glyph.size, flipped: false) { r in
        glyph.draw(in: r)
        NSColor.white.set()
        r.fill(using: .sourceAtop)
        return true
    }

    /// The brand glyph filled with `color`, NON-template, at the idle geometry (no pill) — the
    /// crossfade-safe idle frame (a template would blend as raw black).
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

    /// A state icon: `tint == nil` → idle, the template glyph alone (macOS tints it to the bar).
    /// `tint != nil` → the white glyph on a colored capsule filled at `pillAlpha` (1 = solid;
    /// < 1 = the breathing pulse — only the pill fades, the glyph stays fully opaque). The glyph
    /// keeps the exact same place + size across all states, so switching never shifts the item.
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
        img.isTemplate = (tint == nil)  // idle tints to the bar; active keeps its color
        return img
    }
}

/// Sets the dockless accessory activation policy (no Dock icon) and registers the
/// app as a login item so the menu-bar icon is present at login. The app starts
/// and stops the engine in-process via the C ABI — it owns the engine's lifecycle.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        registerLoginItem()
        useBundledOnnxRuntimeIfPresent()
        useBundledKokoroCoreMLIfPresent()
        useBundledSeparatorIfPresent()
        // Host the engine IN-PROCESS: caps loop + RPC server + TTS queue run on a
        // background thread inside THIS app, so Accessibility / Input-Monitoring /
        // Mic are all granted to the one signed DontSpeak.app bundle. The MCP and
        // the speak/narrate hooks connect to the socket this serves.
        _ = ds_engine_start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Quit truly stops everything (clears the run flag, joins the engine
        // thread, releases any held Caps key, tears down the warm Kokoro child).
        _ = ds_engine_stop()
    }

    /// Distributed (notarized) builds bundle libonnxruntime.dylib in Contents/Frameworks so it's
    /// signed + notarized with the app (no runtime download → no Gatekeeper-quarantine block).
    /// Point `ort` at it before the engine starts; the engine (and the helper child, which inherits
    /// this env) prefers a pre-set ORT_DYLIB_PATH. No-op in local builds where it isn't bundled —
    /// the engine then downloads it on first launch exactly as before.
    private func useBundledOnnxRuntimeIfPresent() {
        guard let dylib = Bundle.main.privateFrameworksURL?.appendingPathComponent("libonnxruntime.dylib"),
            FileManager.default.fileExists(atPath: dylib.path)
        else { return }
        setenv("ORT_DYLIB_PATH", dylib.path, 1)
    }

    /// Point the helper at the bundled FluidAudio Core ML / ANE Kokoro shim
    /// (`libsmkokoro.dylib`) when present, so `tts_provider=apple-native` works in the
    /// installed app. The helper inherits this env and dlopens it; absent (e.g. Intel
    /// builds, where it isn't bundled) the helper falls back to the ONNX path.
    private func useBundledKokoroCoreMLIfPresent() {
        guard let dylib = Bundle.main.privateFrameworksURL?.appendingPathComponent("libsmkokoro.dylib"),
            FileManager.default.fileExists(atPath: dylib.path)
        else { return }
        setenv("SMKOKORO_DYLIB_PATH", dylib.path, 1)
    }

    /// Point the helper at the bundled speaker-SEPARATION model (`sepformer_int8.onnx` in
    /// Resources), used by the dictation speaker-lock to isolate the enrolled voice from a
    /// co-channel background voice. The helper inherits this env; absent → the lock fails
    /// open (transcribes unfiltered), so dictation still works without it.
    private func useBundledSeparatorIfPresent() {
        guard let model = Bundle.main.resourceURL?.appendingPathComponent("sepformer_int8.onnx"),
            FileManager.default.fileExists(atPath: model.path)
        else { return }
        setenv("DONTSPEAK_SEPARATOR_PATH", model.path, 1)
    }

    /// Register DontSpeak.app as a Login Item via SMAppService so the tray icon comes back
    /// at login. Idempotent and fail-quiet (an unregistered/standalone build, or a denied
    /// registration, just means no auto-launch).
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
