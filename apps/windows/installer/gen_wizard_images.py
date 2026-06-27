#!/usr/bin/env python3
"""Generate the Inno Setup modern-wizard images for DontSpeak.

CONSISTENT, not a one-off: the glyph is rasterized from the ACTUAL app-icon SVG
(apps/macos/AppIcon.icon/Assets/Foreground.svg) and the gradient is read straight
from assets/icon.svg. Nothing about the brand mark is re-encoded here, so these
images stay in sync with the app icon automatically when it changes.

Requires ImageMagick with an SVG delegate that handles nested <svg> positioning
(rsvg/cairo — the built-in MSVG renderer drops the inner </> strokes, leaving an
empty balloon). `magick -version` should list `rsvg` under Delegates.

Outputs (24-bit BMP, beside this script):
  wizard-large.bmp / wizard-large-2x.bmp  -> WizardImageFile (left panel, 164x314 @1x)
  wizard-small.bmp / wizard-small-2x.bmp  -> WizardSmallImageFile (top-right, 55x55 @1x)

Run:  python apps/windows/installer/gen_wizard_images.py
"""
import re
import subprocess
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]                       # apps/windows/installer -> repo root
ICON_SVG = REPO / "assets" / "icon.svg"
FG_SVG = REPO / "apps" / "macos" / "AppIcon.icon" / "Assets" / "Foreground.svg"
WHITE = (255, 255, 255)


def _grad_colors():
    """Brand gradient = the two <linearGradient> stops from the real app icon."""
    txt = ICON_SVG.read_text(encoding="utf-8")
    hexes = re.findall(r'stop-color="#([0-9A-Fa-f]{6})"', txt)
    rgb = lambda h: tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))
    return rgb(hexes[0]), rgb(hexes[1])


GRAD_TOP, GRAD_BOT = _grad_colors()


def _glyph():
    """Rasterize the real foreground SVG to a tight, transparent RGBA image."""
    out = HERE / "_glyph_render.png"
    subprocess.run(["magick", "-background", "none", "-density", "700", str(FG_SVG),
                    "-trim", "+repage", str(out)], check=True)
    img = Image.open(out).convert("RGBA")
    out.unlink()
    return img


GLYPH = _glyph()


def _icon():
    """Rasterize the FULL app icon (rounded gradient squircle + glyph) to a tight,
    transparent RGBA — the rounded corners stay transparent so it blends on the header."""
    out = HERE / "_icon_render.png"
    subprocess.run(["magick", "-background", "none", "-density", "384", str(ICON_SVG),
                    "-trim", "+repage", str(out)], check=True)
    img = Image.open(out).convert("RGBA")
    out.unlink()
    return img


ICON = _icon()


def _font(px):
    for name in ("segoeuib.ttf", "seguisb.ttf", "arialbd.ttf"):
        try:
            return ImageFont.truetype(name, px)
        except OSError:
            continue
    return ImageFont.load_default()


def _vgrad(w, h):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        row = tuple(round(GRAD_TOP[i] + (GRAD_BOT[i] - GRAD_TOP[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = row
    return img


def _paste_glyph(img, frac_w, cy_frac):
    gw = round(img.width * frac_w)
    gh = round(gw * GLYPH.height / GLYPH.width)
    g = GLYPH.resize((gw, gh), Image.LANCZOS)
    img.paste(g, ((img.width - gw) // 2, round(img.height * cy_frac) - gh // 2), g)


def large(scale):
    w, h = 164 * scale, 314 * scale
    img = _vgrad(w, h)
    _paste_glyph(img, 0.52, 0.36)
    f = _font(round(23 * scale))
    d = ImageDraw.Draw(img)
    tb = d.textbbox((0, 0), "DontSpeak", font=f)
    d.text(((w - (tb[2] - tb[0])) // 2, round(h * 0.60)), "DontSpeak", font=f, fill=WHITE)
    return img


def small(scale):
    """Top-right header icon: the rounded app tile on a TRANSPARENT canvas, sized down
    with padding so it sits neatly on (and blends into) the white wizard header."""
    s = 55 * scale
    canvas = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    side = round(s * 0.82)                      # padding around the tile
    ic = ICON.resize((side, side), Image.LANCZOS)
    canvas.paste(ic, ((s - side) // 2, (s - side) // 2), ic)
    return canvas


# Welcome panel: full-bleed BMP. Small header icon: PNG so its transparency blends into
# the header (Inno 6.3+ accepts PNG for the wizard images).
for name, im in (("wizard-large", large(1)), ("wizard-large-2x", large(2))):
    im.convert("RGB").save(HERE / f"{name}.bmp")
    print("wrote", name + ".bmp", im.size)
for name, im in (("wizard-small", small(1)), ("wizard-small-2x", small(2))):
    im.save(HERE / f"{name}.png")
    print("wrote", name + ".png", im.size)
