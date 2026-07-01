using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace DontSpeak;

/// <summary>
/// The DontSpeak Windows tray mark — the line-art speech bubble with the code "</>" inside,
/// drawn straight from <c>assets/tray-icon.svg</c> (the single brand source; the macOS app
/// reads the same shape). It is ONE color (the SVG's <c>currentColor</c>), rendered via GDI+
/// at runtime. There is NO background tile in any state — Microsoft's notification-area
/// guidance is a monochrome glyph with NO padding that fills the 16px cell edge-to-edge, so
/// the mark is fit to the whole cell and the engine state is shown by RECOLORING the ink:
///   idle      = theme-foreground ink (template-style auto-tint, like a native tray glyph);
///   recording = mic-ORANGE ink (STT active);
///   speaking  = brand SEED-PURPLE ink (TTS active).
/// The mark keeps the SAME shape and size in every state — only its color changes — so it
/// always renders at the maximum size the cell allows. The notification-area
/// <see cref="TrayIcon"/> wraps these pixels in an HICON.
/// </summary>
internal static class BrandGlyph
{
    // tray-icon.svg stroke width, in the SVG's own coordinate space. The geometry in
    // BuildMark is transcribed verbatim from that file so the Windows mark and the SVG never
    // drift apart; the mark is then fit to the cell by its stroked bounds (no viewBox needed).
    // Heavier than the SVG's own 30 so the thin line-art reads as visually as large as the
    // neighbouring solid Fluent system icons (network/volume) — a 16px tray cell makes a thin
    // stroke look small even when the bounds fill the cell.
    private const float StrokeW = 46f;

    /// <summary>Render the mark into a <paramref name="size"/>×<paramref name="size"/>
    /// straight-alpha BGRA buffer (top-down rows — what an HICON DIB expects): the one-color
    /// <paramref name="ink"/> line-art bubble + "&lt;/&gt;", fit to FILL the cell (no tile, no
    /// padding). When <paramref name="muted"/>, a diagonal "muted" slash is drawn across the
    /// glyph (the Windows analogue of the macOS slashed menu-bar icon): a clear knockout
    /// channel cut through the mark with the ink slash in it, so it reads as crossed-out at
    /// tray size in either theme.</summary>
    internal static byte[] RenderBgra(int size, Color ink, bool muted)
    {
        int w = size, h = size;
        var buf = new byte[w * h * 4];
        using var src = new Bitmap(w, h, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(src))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.PixelOffsetMode = PixelOffsetMode.HighQuality; // crisp edges at 16–20px
            g.Clear(Color.Transparent);

            // The line-art mark (bubble outline + "</>") in one ink color, fit to fill the
            // whole cell. We measure the mark's STROKED bounds (round caps included) and scale
            // those to the cell minus a hair, so the glyph is as large as possible without the
            // anti-aliased caps clipping at the edge. No tile: the state is the ink color.
            using var mark = BuildMark();
            using var pen = new Pen(ink, StrokeW)
            { StartCap = LineCap.Round, EndCap = LineCap.Round, LineJoin = LineJoin.Round };

            var b = mark.GetBounds(new Matrix(), pen); // tight bounds incl. the stroke
            float margin = size * 0.015f;              // hug the cell edges; just enough so AA caps don't clip
            float avail = size - 2f * margin;
            float scale = avail / Math.Max(b.Width, b.Height); // fit by the longer axis → fills the cell
            float offX = margin + (avail - b.Width * scale) / 2f - b.X * scale;
            float offY = margin + (avail - b.Height * scale) / 2f - b.Y * scale;
            using (var m = new Matrix(scale, 0f, 0f, scale, offX, offY))
                mark.Transform(m);

            pen.Width = StrokeW * scale; // scale the stroke to match the fitted geometry
            g.DrawPath(pen, mark);

            if (muted)
            {
                // Diagonal slash, top-left → bottom-right, inset from the corners. First cut a
                // CLEAR channel through the glyph (SourceCopy with Transparent erases pixels),
                // then lay the ink slash inside it — so the slash reads distinctly over the
                // mark instead of blending into its strokes (mirrors the macOS slashed icon).
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

    /// <summary>Idle ink color = the theme foreground (dark in light mode, light in dark
    /// mode) — the Windows analogue of the macOS isTemplate auto-tint, so idle never reads
    /// as "disabled". Used only for the idle state.</summary>
    internal static Color IdleForeground()
    {
        using var k = Registry.CurrentUser.OpenSubKey(
            @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        bool light = k?.GetValue("SystemUsesLightTheme") is int v && v != 0;
        return light ? Color.FromArgb(255, 40, 40, 45) : Color.FromArgb(255, 236, 236, 240);
    }

    /// <summary>Build the line-art mark — the bubble (closed) + the "</>" (three open
    /// sub-figures) — in one <see cref="GraphicsPath"/> in raw assets/tray-icon.svg coordinates
    /// (transcribed VERBATIM). The caller measures its stroked bounds and fits it to the cell,
    /// so no viewBox mapping happens here.</summary>
    private static GraphicsPath BuildMark()
    {
        static PointF P(float x, float y) => new(x, y); // raw SVG coords; caller fits them
        var p = new GraphicsPath();

        // bubble outline (closed) — same path the macOS bubble uses, here stroked not filled
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

        // "</>" — three OPEN sub-figures so the round end-caps show (the playful brand glyph)
        p.StartFigure(); p.AddLines(new[] { P(218, 205), P(168, 250), P(218, 295) }); // <
        p.StartFigure(); p.AddLine(P(274, 178), P(238, 322));                          // /
        p.StartFigure(); p.AddLines(new[] { P(292, 205), P(342, 250), P(292, 295) }); // >

        return p;
    }
}
