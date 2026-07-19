//! Tray status-icon: rasterize canonical `assets/tray-icon.svg`, brand tint, muted slash
//! (Windows `BrandGlyph.cs` geometry). SNI pixmap ARGB32 NBO — theme icons can't match
//! glyph + tint + slash across hosts.

use std::sync::OnceLock;

use resvg::tiny_skia;
use resvg::usvg;

/// Canonical glyph, embedded at build time (one asset, all hosts).
const TRAY_SVG: &str = include_str!("../../../../assets/tray-icon.svg");

/// Parsed once; `tray.rs` calls `render` ~4× per status push.
static TRAY_TREE: OnceLock<usvg::Tree> = OnceLock::new();

fn tray_tree() -> &'static usvg::Tree {
    TRAY_TREE.get_or_init(|| {
        usvg::Tree::from_str(TRAY_SVG, &usvg::Options::default()).unwrap_or_else(|_| {
            // Empty tree → zero coverage → blank icon.
            usvg::Tree::from_str(
                "<svg xmlns='http://www.w3.org/2000/svg'/>",
                &usvg::Options::default(),
            )
            .expect("fallback SVG is valid")
        })
    })
}

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Idle tray foreground. GNOME Shell / Ubuntu panels are dark regardless of app theme, and a
/// pixmap can't be recolored by the host — light fg so idle reads on the panel. Analogue of
/// Windows `BrandGlyph.IdleForeground` (taskbar theme, not app window).
pub fn idle_fg() -> Rgb {
    Rgb(0xEC, 0xEC, 0xF0)
}

/// `seed_purple` + `mic_orange` from BRAND_COLORS_JSON. Falls back to the same brand hex the
/// Swift/C# hosts hardcode if the engine is down or JSON is malformed.
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

/// SNI pixmap (ARGB32 NBO): brand glyph tinted with `ink`; muted slash when `muted`.
pub fn render(size: u32, ink: Rgb, muted: bool) -> ksni::Icon {
    let mut pm = tiny_skia::Pixmap::new(size, size).expect("size is non-zero");

    // Rasterize SVG; keep only coverage (source color is irrelevant — recolor below).
    {
        let tree = tray_tree();
        let svg = tree.size();
        let margin = size as f32 * 0.05;
        let avail = size as f32 - 2.0 * margin;
        let scale = avail / svg.width().max(svg.height());
        let tx = (size as f32 - svg.width() * scale) / 2.0;
        let ty = (size as f32 - svg.height() * scale) / 2.0;
        let transform = tiny_skia::Transform::from_translate(tx, ty).pre_scale(scale, scale);
        resvg::render(tree, transform, &mut pm.as_mut());
    }

    for px in pm.pixels_mut() {
        let a = px.alpha();
        let pre = |c: u8| ((c as u16 * a as u16) / 255) as u8;
        if let Some(p) =
            tiny_skia::PremultipliedColorU8::from_rgba(pre(ink.0), pre(ink.1), pre(ink.2), a)
        {
            *px = p;
        }
    }

    // Muted slash — same geometry as Windows BrandGlyph.cs: TL→BR, 13% inset; knock out a
    // transparent gap (~2× ink width), then lay the ink slash inside.
    if muted {
        let inset = size as f32 * 0.13;
        let path = {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(inset, inset);
            pb.line_to(size as f32 - inset, size as f32 - inset);
            pb.finish()
        };
        if let Some(path) = path {
            let gap = tiny_skia::Paint {
                blend_mode: tiny_skia::BlendMode::Clear,
                ..Default::default()
            };
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
            pm.stroke_path(
                &path,
                &paint,
                &stroke,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    // Premultiplied RGBA → straight ARGB32 NBO (A, R, G, B per pixel).
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
