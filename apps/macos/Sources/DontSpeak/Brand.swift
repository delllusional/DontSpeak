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
        guard let ptr = ds_brand_colors_json() else { return [:] }
        defer { ds_string_free(ptr) }
        guard let data = String(cString: ptr).data(using: .utf8),
              let map = (try? JSONSerialization.jsonObject(with: data)) as? [String: String]
        else { return [:] }
        return map
    }()

    /// Parse "#RRGGBB" → NSColor (sRGB). nil on a malformed value.
    private static func parse(_ hex: String?) -> NSColor? {
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
        parse(colors[key]) ?? fallback
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
}

extension Color {
    /// SwiftUI alias for the shared [`Brand.warning`] orange — used by the warming status
    /// dot and the dictation panel's no-focus glow so they share ONE source of truth.
    static let smWarning = Color(nsColor: Brand.warning)
}
