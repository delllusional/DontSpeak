using System;
using System.Text.Json;
using WinColor = Windows.UI.Color;
using GdiColor = System.Drawing.Color;

namespace DontSpeak;

/// <summary>
/// Brand tints — the SAME cross-platform source the macOS app reads (see
/// <c>macos/Sources/DontSpeak/Brand.swift</c>): colors come from the Rust core via
/// <see cref="Native.BrandColorsJson"/> (<c>ds_brand_colors_json</c>), with the
/// brand hexes as a fallback, so every platform's UI tints identically without each
/// hardcoding its own copy. Resolved once and cached. Each tint is exposed in both color
/// worlds this app spans: <c>Windows.UI.Color</c> for the WinUI/XAML window and
/// <c>System.Drawing.Color</c> for the GDI+ tray glyph and dictation overlay.
/// </summary>
internal static class Brand
{
    /// <summary>Icon seed / "speaking" tint — brand purple (#5B4397).</summary>
    public static readonly WinColor SeedPurple;
    /// <summary>"Recording" tint — mic orange (#FF9F0A).</summary>
    public static readonly WinColor MicOrange;
    /// <summary>Warming / blocked / downloading dots AND the dictation no-target glow (#FF9F0A).</summary>
    public static readonly WinColor Warning;

    /// <summary><see cref="SeedPurple"/> as a GDI+ color (the tray "speaking" glyph ink).</summary>
    public static GdiColor SeedPurpleGdi => Gdi(SeedPurple);
    /// <summary><see cref="MicOrange"/> as a GDI+ color (the tray "recording" glyph ink).</summary>
    public static GdiColor MicOrangeGdi => Gdi(MicOrange);
    /// <summary><see cref="Warning"/> as a GDI+ color (the dictation no-target glow).</summary>
    public static GdiColor WarningGdi => Gdi(Warning);

    /// <summary>Logs-tab source palette — distinct, theme-neutral colors from the SAME shared
    /// Rust source (<c>ds_log_colors_json</c>) every platform reads; a UI assigns each
    /// source the entry at its first-appearance index. Brand-hex fallback if the engine returns
    /// "{}".</summary>
    public static readonly WinColor[] LogSourcePalette;
    private static readonly System.Collections.Generic.Dictionary<string, WinColor> LogLevelColors =
        new(StringComparer.Ordinal);

    /// <summary>The color for a log level (ERROR / WARN), or null for INFO / unknown (which render
    /// in the default text color). Resolved from the shared Rust log-colors source.</summary>
    public static WinColor? LogLevelColor(string level) =>
        LogLevelColors.TryGetValue(level, out var c) ? c : null;

    static Brand()
    {
        // Hardcoded fallbacks, identical to Brand.swift — used if the engine returns "{}".
        SeedPurple = FromHex("#5B4397");
        MicOrange = FromHex("#FF9F0A");
        Warning = FromHex("#FF9F0A");
        try
        {
            using var doc = JsonDocument.Parse(Native.BrandColorsJson());
            var root = doc.RootElement;
            SeedPurple = Hex(root, "seed_purple", SeedPurple);
            MicOrange = Hex(root, "mic_orange", MicOrange);
            Warning = Hex(root, "warning", Warning);
        }
        catch { /* engine down / malformed → keep the brand-hex fallbacks */ }

        // Logs-tab colors — the sibling shared source (ds_log_colors_json). Fallbacks
        // mirror the Rust defaults so coloring still works if the engine returns "{}".
        WinColor[] palette =
        {
            FromHex("#8B7BD8"), FromHex("#3FA7A1"), FromHex("#5B8DEF"), FromHex("#4CAF6E"),
            FromHex("#D97FB0"), FromHex("#CB8A3E"), FromHex("#49B6C2"), FromHex("#B07BD8"),
        };
        LogLevelColors["ERROR"] = FromHex("#E84646");
        LogLevelColors["WARN"] = FromHex("#FF9F0A");
        try
        {
            using var doc = JsonDocument.Parse(Native.LogColorsJson());
            var root = doc.RootElement;
            if (root.TryGetProperty("source_palette", out var pal) && pal.ValueKind == JsonValueKind.Array)
            {
                var list = new System.Collections.Generic.List<WinColor>();
                foreach (var item in pal.EnumerateArray())
                    if (item.ValueKind == JsonValueKind.String && item.GetString() is string s)
                        list.Add(FromHex(s));
                if (list.Count > 0) palette = list.ToArray();
            }
            if (root.TryGetProperty("levels", out var lv) && lv.ValueKind == JsonValueKind.Object)
                foreach (var p in lv.EnumerateObject())
                    if (p.Value.ValueKind == JsonValueKind.String && p.Value.GetString() is string s)
                        LogLevelColors[p.Name] = FromHex(s);
        }
        catch { /* engine down / malformed → keep the fallback palette + level colors */ }
        LogSourcePalette = palette;
    }

    private static GdiColor Gdi(WinColor c) => GdiColor.FromArgb(c.A, c.R, c.G, c.B);

    private static WinColor Hex(JsonElement e, string k, WinColor fallback) =>
        e.TryGetProperty(k, out var v) && v.ValueKind == JsonValueKind.String && v.GetString() is string s
            ? FromHex(s) : fallback;

    /// <summary>Parse "#RRGGBB" (alpha forced opaque); returns a visible magenta on garbage.</summary>
    private static WinColor FromHex(string hex)
    {
        var h = hex.TrimStart('#');
        if (h.Length == 6
            && byte.TryParse(h.AsSpan(0, 2), System.Globalization.NumberStyles.HexNumber, null, out var r)
            && byte.TryParse(h.AsSpan(2, 2), System.Globalization.NumberStyles.HexNumber, null, out var g)
            && byte.TryParse(h.AsSpan(4, 2), System.Globalization.NumberStyles.HexNumber, null, out var b))
            return WinColor.FromArgb(0xFF, r, g, b);
        return WinColor.FromArgb(0xFF, 0xFF, 0x00, 0xFF);
    }
}
