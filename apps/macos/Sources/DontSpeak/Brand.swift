//  Brand.swift
//
//  Brand colors — read from the SHARED cross-platform source of truth
//  (`ds_brand_colors_json` in ds-core, the SAME C ABI the Windows app uses),
//  so a tint can't drift between platforms. The hardcoded hexes here are only a fallback
//  for an FFI failure; the real values live in Rust (`ds_core::BRAND_COLORS_JSON`).

import AppKit
import CDontSpeak
import SwiftUI

enum Brand {
    /// The shared brand-color map (key → "#RRGGBB"), read ONCE from ds-core.
    private static let colors: [String: String] = {
        guard let json = ffiString(ds_brand_colors_json), let data = json.data(using: .utf8),
              let map = (try? JSONSerialization.jsonObject(with: data)) as? [String: String]
        else { return [:] }
        return map
    }()

    /// Parse "#RRGGBB" → NSColor (sRGB); nil on a malformed value. The ONE hex parser shared by
    /// every color read from the shared Rust source (brand tints AND the Logs-tab palette), so
    /// the parsing rule lives in one place.
    static func nsColor(fromHex hex: String?) -> NSColor? {
        guard let hex, hex.hasPrefix("#"), hex.count == 7,
              let v = Int(hex.dropFirst(), radix: 16) else { return nil }
        return NSColor(
            srgbRed: CGFloat((v >> 16) & 0xFF) / 255.0,
            green: CGFloat((v >> 8) & 0xFF) / 255.0,
            blue: CGFloat(v & 0xFF) / 255.0,
            alpha: 1.0
        )
    }

    private static func color(_ key: String, fallback: NSColor) -> NSColor {
        nsColor(fromHex: colors[key]) ?? fallback
    }

    /// The DontSpeak **seed color** (`#5B4397`): the icon-generation seed + the menu-bar
    /// "speaking" pill. See `assets/seed-color.txt` / `AppIcon.icon/icon.json`.
    static let seedPurple = color("seed_purple", fallback: NSColor(srgbRed: 0.357, green: 0.263, blue: 0.592, alpha: 1.0))

    /// The macOS microphone-in-use "recording" orange (`#FF9F0A`) — the menu-bar pill,
    /// matching the system's own privacy cue.
    static let micOrange = color("mic_orange", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    /// The warning / warming accent (`#FF9F0A`): the warming/blocked/downloading status
    /// dots AND the dictation panel's no-focus glow — one orange for "attention / not ready".
    static let warning = color("warning", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    // MARK: - Logs-tab colors (shared source)
    //
    // The Logs view's coloring comes from the SAME cross-platform source as the brand tints
    // (`ds_log_colors_json` in ds-core), so every platform's activity log tints identically —
    // the macOS analogue of the Windows `Brand.LogSourcePalette` / `Brand.LogLevelColor`.

    /// The decoded `{levels:{ERROR,WARN}, source_palette:[…]}` map, read ONCE from ds-core.
    private static let logColors: (levels: [String: String], palette: [String]) = {
        guard let json = ffiString(ds_log_colors_json), let data = json.data(using: .utf8),
              let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return ([:], []) }
        let levels = (obj["levels"] as? [String: String]) ?? [:]
        let palette = (obj["source_palette"] as? [String]) ?? []
        return (levels, palette)
    }()

    /// The ordered, theme-neutral per-source palette — the Logs view assigns each distinct log
    /// source the palette color at its first-appearance index (so colors are stable + identical
    /// on every platform reading the same lines). Empty ⇒ the view falls back to the text color.
    static let logSourcePalette: [NSColor] = logColors.palette.compactMap { nsColor(fromHex: $0) }

    /// The color for a log `level`: ERROR / WARN from the shared map; `nil` for INFO / unknown
    /// (the line keeps the default text color).
    static func logLevelColor(_ level: String) -> NSColor? {
        nsColor(fromHex: logColors.levels[level])
    }
}

extension Color {
    /// SwiftUI alias for the shared [`Brand.warning`] orange — used by the warming status
    /// dot and the dictation panel's no-focus glow so they share ONE source of truth.
    static let smWarning = Color(nsColor: Brand.warning)
}
