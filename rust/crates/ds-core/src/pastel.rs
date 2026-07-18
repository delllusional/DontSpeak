//! Usage-card speaking wash: one pastel recipe for every host.
//!
//! HSV with fixed S/V, random H; wash α matches the former brand-purple overlay.
//! Hosts call [`crate::ffi::ds_random_pastel_wash_json`] and only paint — no local HSV.

/// Saturation of the pastel base (0…1).
pub const PASTEL_S: f64 = 0.42;
/// Value/brightness of the pastel base (0…1).
pub const PASTEL_V: f64 = 0.92;
/// Overlay alpha applied by hosts on the speaking Usage card.
pub const WASH_ALPHA: f64 = 0.30;

/// Opaque sRGB from fixed-S/V pastel with random H.
pub fn random_pastel_rgb() -> (u8, u8, u8) {
    hsv_to_rgb(random_unit(), PASTEL_S, PASTEL_V)
}

/// JSON for hosts: `{"r","g","b","a"}` — opaque RGB + wash alpha. One roll per call.
pub fn random_pastel_wash_json() -> String {
    let (r, g, b) = random_pastel_rgb();
    format!(r#"{{"r":{r},"g":{g},"b":{b},"a":{WASH_ALPHA}}}"#)
}

/// HSV → sRGB (`h` in \[0, 1), `s`/`v` in \[0, 1\]).
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(1.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - f64::from(i);
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

fn random_unit() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TICK: AtomicU64 = AtomicU64::new(0);
    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    TICK.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
    (hasher.finish() as f64) / (u64::MAX as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wash_json_has_rgb_and_alpha() {
        let v: serde_json::Value = serde_json::from_str(&random_pastel_wash_json()).unwrap();
        for k in ["r", "g", "b"] {
            let n = v[k].as_u64().unwrap();
            assert!(n <= 255, "{k}={n}");
        }
        assert!((v["a"].as_f64().unwrap() - WASH_ALPHA).abs() < 1e-9);
    }

    #[test]
    fn hsv_red_and_white_corners() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv_to_rgb(0.0, 0.0, 1.0), (255, 255, 255));
    }

    #[test]
    fn fixed_sv_pastels_stay_bright() {
        // V=0.92 → min channel after S still high enough to read as soft, not dark.
        for i in 0..12 {
            let h = f64::from(i) / 12.0;
            let (r, g, b) = hsv_to_rgb(h, PASTEL_S, PASTEL_V);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            assert!(max >= 200, "too dim at h={h}: {r},{g},{b}");
            assert!(min >= 100, "too saturated-dark at h={h}: {r},{g},{b}");
        }
    }

    #[test]
    fn successive_rolls_are_not_identical() {
        // Probabilistic: 8 rolls all equal only if RNG is stuck.
        let mut colors = std::collections::HashSet::new();
        for _ in 0..8 {
            colors.insert(random_pastel_rgb());
        }
        assert!(
            colors.len() > 1,
            "expected variety across rolls, got {colors:?}"
        );
    }
}
