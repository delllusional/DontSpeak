using System;
using System.Text.Json;
using WinColor = Windows.UI.Color;
using GdiColor = System.Drawing.Color;

namespace DontSpeak;

/// <summary>
/// Brand tints from the Rust core via <see cref="Native.BrandColorsJson"/> (same source as
/// <c>macos/.../Brand.swift</c>), with brand-hex fallbacks. Cached once. Exposed as both
/// <c>Windows.UI.Color</c> (WinUI) and <c>System.Drawing.Color</c> (GDI+ tray/overlay).
/// </summary>
internal static class Brand
{
    public static readonly WinColor SeedPurple;   // speaking / icon seed (#5B4397)
    public static readonly WinColor MicOrange;    // recording (#FF9F0A)
    public static readonly WinColor Warning;      // warming/blocked/download + no-target glow

    public static GdiColor SeedPurpleGdi => Gdi(SeedPurple);
    public static GdiColor MicOrangeGdi => Gdi(MicOrange);

    /// <summary>Logs-tab source palette from <c>ds_log_colors_json</c> (shared across platforms);
    /// first-appearance index picks the color. Brand-hex fallback if engine returns "{}".</summary>
    public static readonly WinColor[] LogSourcePalette;
    private static readonly System.Collections.Generic.Dictionary<string, WinColor> LogLevelColors =
        new(StringComparer.Ordinal);

    /// <summary>ERROR/WARN level color from the shared Rust log-colors source; null for INFO/unknown.</summary>
    public static WinColor? LogLevelColor(string level) =>
        LogLevelColors.TryGetValue(level, out var c) ? c : null;

    static Brand()
    {
        // Fallbacks match Brand.swift — used if the engine returns "{}".
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
        catch { /* engine down / malformed → keep fallbacks */ }

        // Fallbacks mirror Rust defaults so coloring works if the engine returns "{}".
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
        catch { /* engine down / malformed → keep fallback palette + level colors */ }
        LogSourcePalette = palette;
    }

    private static GdiColor Gdi(WinColor c) => GdiColor.FromArgb(c.A, c.R, c.G, c.B);

    private static WinColor Hex(JsonElement e, string k, WinColor fallback) =>
        e.TryGetProperty(k, out var v) && v.ValueKind == JsonValueKind.String && v.GetString() is string s
            ? FromHex(s) : fallback;

    /// <summary>Parse "#RRGGBB" (opaque); magenta on garbage so bad hex is visible.</summary>
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
