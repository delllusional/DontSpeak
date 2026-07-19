using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace DontSpeak;

/// <summary>
/// Tray mark from <c>assets/tray-icon.svg</c> (shared with macOS): monochrome line-art bubble
/// + "&lt;/&gt;", edge-to-edge at 16px. State is ink color only (idle = theme fg, recording =
/// mic orange, speaking = seed purple). <see cref="TrayIcon"/> wraps BGRA in an HICON.
/// </summary>
internal static class BrandGlyph
{
    // SVG coordinate-space stroke. BuildMark is transcribed verbatim from tray-icon.svg so
    // Windows and the asset never drift. Heavier than the SVG's 30 so thin line-art reads like
    // neighboring solid Fluent tray icons at 16px.
    private const float StrokeW = 46f;

    /// <summary>size×size straight-alpha BGRA (top-down, HICON DIB). Muted slash: clear
    /// knockout then ink (macOS slashed menu-bar analogue).</summary>
    internal static byte[] RenderBgra(int size, Color ink, bool muted)
    {
        int w = size, h = size;
        var buf = new byte[w * h * 4];
        using var src = new Bitmap(w, h, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(src))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.PixelOffsetMode = PixelOffsetMode.HighQuality;
            g.Clear(Color.Transparent);

            // Fit stroked bounds (round caps included) to the cell minus a hair so AA caps don't clip.
            using var mark = BuildMark();
            using var pen = new Pen(ink, StrokeW)
            { StartCap = LineCap.Round, EndCap = LineCap.Round, LineJoin = LineJoin.Round };

            var b = mark.GetBounds(new Matrix(), pen);
            float margin = size * 0.015f;
            float avail = size - 2f * margin;
            float scale = avail / Math.Max(b.Width, b.Height);
            float offX = margin + (avail - b.Width * scale) / 2f - b.X * scale;
            float offY = margin + (avail - b.Height * scale) / 2f - b.Y * scale;
            using (var m = new Matrix(scale, 0f, 0f, scale, offX, offY))
                mark.Transform(m);

            pen.Width = StrokeW * scale;
            g.DrawPath(pen, mark);

            if (muted)
            {
                // SourceCopy + Transparent cuts a channel; then ink slash (else stroke blends into mark).
                float inset = size * 0.13f;
                float x1 = inset, y1 = inset, x2 = size - inset, y2 = size - inset;
                float sw = StrokeW * scale;
                g.CompositingMode = CompositingMode.SourceCopy;
                using (var gap = new Pen(Color.Transparent, sw * 1.8f)
                { StartCap = LineCap.Round, EndCap = LineCap.Round })
                    g.DrawLine(gap, x1, y1, x2, y2);
                g.CompositingMode = CompositingMode.SourceOver;
                using (var slash = new Pen(ink, sw * 0.9f)
                { StartCap = LineCap.Round, EndCap = LineCap.Round })
                    g.DrawLine(slash, x1, y1, x2, y2);
            }
        }
        var data = src.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        try { Marshal.Copy(data.Scan0, buf, 0, buf.Length); }
        finally { src.UnlockBits(data); }
        return buf;
    }

    /// <summary>Theme foreground ink (light/dark) — macOS isTemplate analogue so idle reads
    /// active. Idle state only.</summary>
    internal static Color IdleForeground()
    {
        bool light;
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            light = k?.GetValue("SystemUsesLightTheme") is int v && v != 0;
        }
        catch { light = false; } // locked-down registry → dark ink
        return light ? Color.FromArgb(255, 40, 40, 45) : Color.FromArgb(255, 236, 236, 240);
    }

    /// <summary>Bubble (closed) + "&lt;/&gt;" (three open figures) in raw tray-icon.svg coords
    /// (verbatim). Caller measures stroked bounds and fits — no viewBox here.</summary>
    private static GraphicsPath BuildMark()
    {
        static PointF P(float x, float y) => new(x, y);
        var p = new GraphicsPath();

        // bubble outline (closed) — same path as macOS; stroked, not filled
        p.StartFigure();
        p.AddBezier(P(270, 90), P(390, 90), P(470, 165), P(470, 250));
        p.AddBezier(P(470, 250), P(470, 335), P(390, 410), P(270, 410));
        p.AddBezier(P(270, 410), P(238, 410), P(205, 404), P(178, 392));
        p.AddLine(P(178, 392), P(115, 425));
        p.AddBezier(P(115, 425), P(102, 432), P(90, 420), P(96, 406));
        p.AddLine(P(96, 406), P(112, 365));
        p.AddBezier(P(112, 365), P(86, 335), P(70, 295), P(70, 250));
        p.AddBezier(P(70, 250), P(70, 165), P(150, 90), P(270, 90));
        p.CloseFigure();

        // "</>" — open figures so round end-caps show
        p.StartFigure(); p.AddLines(new[] { P(218, 205), P(168, 250), P(218, 295) }); // <
        p.StartFigure(); p.AddLine(P(274, 178), P(238, 322)); // /
        p.StartFigure(); p.AddLines(new[] { P(292, 205), P(342, 250), P(292, 295) }); // >

        return p;
    }
}
