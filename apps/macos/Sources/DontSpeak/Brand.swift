// Brand colors from ds_brand_colors_json (same ABI as Windows — tints can't drift).
// Hardcoded hexes are FFI-failure fallbacks.

import AppKit
import CDontSpeak
import SwiftUI

enum Brand {
    /// Key → "#RRGGBB", once from ds-core.
    private static let colors: [String: String] = {
        guard let json = ffiString(ds_brand_colors_json), let data = json.data(using: .utf8),
            let map = (try? JSONSerialization.jsonObject(with: data)) as? [String: String]
        else { return [:] }
        return map
    }()

    /// "#RRGGBB" → sRGB; nil if malformed. Single parser for brand + log colors.
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

    static let seedPurple = color(
        "seed_purple", fallback: NSColor(srgbRed: 0.357, green: 0.263, blue: 0.592, alpha: 1.0))

    static let micOrange = color(
        "mic_orange", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    static let warning = color(
        "warning", fallback: NSColor(srgbRed: 1.0, green: 0.624, blue: 0.039, alpha: 1.0))

    // MARK: - Logs colors (ds_log_colors_json — shared with Windows)

    private static let logColors: (levels: [String: String], palette: [String]) = {
        guard let json = ffiString(ds_log_colors_json), let data = json.data(using: .utf8),
            let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return ([:], []) }
        let levels = (obj["levels"] as? [String: String]) ?? [:]
        let palette = (obj["source_palette"] as? [String]) ?? []
        return (levels, palette)
    }()

    /// First-appearance palette index; empty → default text color.
    static let logSourcePalette: [NSColor] = logColors.palette.compactMap { nsColor(fromHex: $0) }

    /// ERROR/WARN from shared map; nil for INFO/unknown.
    static func logLevelColor(_ level: String) -> NSColor? {
        nsColor(fromHex: logColors.levels[level])
    }
}

extension Color {
    static let smWarning = Color(nsColor: Brand.warning)
    /// Update pill accent (brand purple).
    static let smSeedPurple = Color(nsColor: Brand.seedPurple)
}

extension Brand {
    /// One roll from ds_random_pastel_wash_json. Nil if FFI fails.
    static func randomPastelWash() -> Color? {
        guard let json = ffiString(ds_random_pastel_wash_json),
            let data = json.data(using: .utf8),
            let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
            let r = obj["r"] as? Int, let g = obj["g"] as? Int, let b = obj["b"] as? Int
        else { return nil }
        let a = (obj["a"] as? Double) ?? (obj["a"] as? NSNumber)?.doubleValue ?? 0.30
        return Color(
            .sRGB,
            red: Double(r) / 255.0,
            green: Double(g) / 255.0,
            blue: Double(b) / 255.0,
            opacity: a
        )
    }
}
