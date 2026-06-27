//! Tray status-icon rendering — the Linux analogue of the macOS/Windows custom tray glyph.
//!
//! Unifies the icon LOGIC across all three hosts: it rasterizes the ONE canonical
//! `assets/tray-icon.svg` (the same bubble + `</>` mark the Swift/C# hosts draw), tints it per
//! state with the SHARED brand colors (`ds-core` BRAND_COLORS_JSON — read here through
//! [`crate::ffi::brand_colors_json`], exactly as Brand.swift / Brand.cs do), and overlays the
//! muted slash using the SAME geometry as the Windows host (apps/windows/winui/BrandGlyph.cs).
//! Output is the StatusNotifierItem pixmap format: ARGB32, network byte order.
//!
//! This replaces the old freedesktop-symbolic-name approach (`icon_name`): a custom pixmap is
//! the only way to match the brand glyph + colored tint + slash the other two platforms use.

use resvg::tiny_skia;
use resvg::usvg;

/// The ONE canonical glyph, embedded at build time — reused, never duplicated per platform.
const TRAY_SVG: &str = include_str!("../../../../assets/tray-icon.svg");

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Idle foreground for the tray glyph. Unlike the app window, the GNOME Shell top bar (and
/// Ubuntu's panel) is dark regardless of the light/dark app theme — and a pixmap, unlike a
/// symbolic theme icon, can't be recolored by the host. So idle uses a light foreground that
/// reads on the dark panel (the recording/speaking states are brand-colored and read on dark
/// too). This is the panel-theme analogue of the Windows host's `BrandGlyph.IdleForeground`,
/// which likewise keys off the *taskbar* theme rather than the app window.
pub fn idle_fg() -> Rgb {
    Rgb(0xEC, 0xEC, 0xF0)
}

/// Parse `seed_purple` + `mic_orange` from the shared BRAND_COLORS_JSON. Falls back to the
/// same brand hex the Swift/C# hosts hardcode if the engine is down or the JSON is malformed.
pub fn brand_colors(json: &str) -> (Rgb, Rgb) {
    let map: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let pick = |k: &str, d: Rgb| {
        map.get(k)
            .and_then(|v| v.as_str())
            .and_then(parse_hex)
            .unwrap_or(d)
    };
    (
        pick("seed_purple", Rgb(0x5B, 0x43, 0x97)),
        pick("mic_orange", Rgb(0xFF, 0x9F, 0x0A)),
    )
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// Render the brand glyph at `size`×`size`, tinted `ink`, with a muted slash when `muted`.
/// Returns a ksni pixmap (ARGB32, network byte order).
pub fn render(size: u32, ink: Rgb, muted: bool) -> ksni::Icon {
    let mut pm = tiny_skia::Pixmap::new(size, size).expect("size is non-zero");

    // 1. Rasterize the SVG; we keep only its coverage (alpha) — the source color is irrelevant.
    if let Ok(tree) = usvg::Tree::from_str(TRAY_SVG, &usvg::Options::default()) {
        let svg = tree.size();
        let margin = size as f32 * 0.05;
        let avail = size as f32 - 2.0 * margin;
        let scale = avail / svg.width().max(svg.height());
        let tx = (size as f32 - svg.width() * scale) / 2.0;
        let ty = (size as f32 - svg.height() * scale) / 2.0;
        let transform = tiny_skia::Transform::from_translate(tx, ty).pre_scale(scale, scale);
        resvg::render(&tree, transform, &mut pm.as_mut());
    }

    // 2. Recolor: each pixel becomes `ink` premultiplied by the rendered coverage.
    for px in pm.pixels_mut() {
        let a = px.alpha();
        let pre = |c: u8| ((c as u16 * a as u16) / 255) as u8;
        if let Some(p) =
            tiny_skia::PremultipliedColorU8::from_rgba(pre(ink.0), pre(ink.1), pre(ink.2), a)
        {
            *px = p;
        }
    }

    // 3. Muted slash — same geometry as Windows (BrandGlyph.cs): TL→BR, 13% inset, knock out a
    //    transparent "gap" (≈2× the ink width) first, then lay the ink slash inside it.
    if muted {
        let inset = size as f32 * 0.13;
        let path = {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(inset, inset);
            pb.line_to(size as f32 - inset, size as f32 - inset);
            pb.finish()
        };
        if let Some(path) = path {
            let mut gap = tiny_skia::Paint::default();
            gap.blend_mode = tiny_skia::BlendMode::Clear;
            let mut stroke = tiny_skia::Stroke {
                width: size as f32 * 0.186,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            pm.stroke_path(&path, &gap, &stroke, tiny_skia::Transform::identity(), None);

            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(ink.0, ink.1, ink.2, 255);
            paint.anti_alias = true;
            stroke.width = size as f32 * 0.093;
            pm.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
        }
    }

    // 4. Premultiplied RGBA → straight ARGB32, network byte order (A, R, G, B per pixel).
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for px in pm.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}
