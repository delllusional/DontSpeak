// Brand colors from the shared cross-platform map (`ds_brand_colors_json` / ds-core) —
// same C ABI as Windows, so tints can't drift. Hardcoded hexes are FFI-failure fallbacks.

import AppKit
import CDontSpeak
import SwiftUI

enum Brand {
    /// Key → "#RRGGBB", read once from ds-core.
    private static let colors: [String: String] = {
        guard let json = ffiString(ds_brand_colors_json), let data = json.data(using: .utf8),
            let map = (try? JSONSerialization.jsonObject(with: data)) as? [String: String]
        else { return [:] }
        return map
    }()

    /// "#RRGGBB" → sRGB NSColor; nil if malformed. Single hex parser for brand + log colors.
    static func nsColor(fromHex hex: String?) -> NSColor? {
        guard let hex, hex.hasPrefix("#"), hex.count == 7,
            let v = Int(hex.dropFirst(), radix: 16)
        else { return nil }
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

    /// Seed / menu-bar "speaking" pill (`#5B4397`). See `assets/seed-color.txt`.
    static let seedPurple = color(
        "seed_purple", fallback: NSColor(srgbRed: 0.357, green: 0.263, blue: 0.592, alpha: 1.0))

    /// Menu-bar recording pill — system mic-in-use orange (`#FF9F0A`).
    static let micOrange = color(
        "mic_orange", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    /// Warming / blocked / no-focus "attention" orange (`#FF9F0A`).
    static let warning = color(
        "warning", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    // MARK: - Logs-tab colors (`ds_log_colors_json` — same shared source as Windows)

    private static let logColors: (levels: [String: String], palette: [String]) = {
        guard let json = ffiString(ds_log_colors_json), let data = json.data(using: .utf8),
            let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return ([:], []) }
        let levels = (obj["levels"] as? [String: String]) ?? [:]
        let palette = (obj["source_palette"] as? [String]) ?? []
        return (levels, palette)
    }()

    /// Per-source palette; Logs assigns first-appearance index. Empty → default text color.
    static let logSourcePalette: [NSColor] = logColors.palette.compactMap { nsColor(fromHex: $0) }

    /// ERROR / WARN from the shared map; nil for INFO / unknown.
    static func logLevelColor(_ level: String) -> NSColor? {
        nsColor(fromHex: logColors.levels[level])
    }
}

extension Color {
    /// Shared warning orange (warming dot + dictation no-focus glow).
    static let smWarning = Color(nsColor: Brand.warning)

    /// Brand accent for neutral "notice me" cues (e.g. update-available pill) — not a warning.
    static let smSeedPurple = Color(nsColor: Brand.seedPurple)
}
