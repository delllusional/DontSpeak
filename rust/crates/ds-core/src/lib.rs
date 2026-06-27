//! ds-core — the UI-AGNOSTIC core of DontSpeak.
//!
//! A THIN bridge for the menu-bar/health UI: it reports engine liveness
//! ([`engine`]), model presence ([`models`]), and the engine's model-status JSON,
//! and it also hosts the in-process engine lifecycle (`ds_engine_start`/
//! `_stop`/`_reload`) plus the locale/provider controls the host app drives —
//! all over a tiny stable C ABI ([`ffi`], the committed `dontspeak.h`). The
//! remaining control surface (voice/engine/language/rate/toggles/downloads)
//! lives in DontSpeak, so the rich config/voice-picker core that used to
//! live here is gone.
//!   - macOS: the SwiftUI app (`../macos/`) links the staticlib.
//!   - Win/Linux later: a native UI binds the same header (cdylib).

pub mod engine;
pub mod ffi;
pub(crate) mod host;
pub mod models;
pub mod status_fmt;

/// Product homepage — the single source of truth every platform's UI links to
/// (macOS About screen, the WinUI app). Exposed over the C ABI as
/// `ds_homepage_url()`.
pub const HOMEPAGE_URL: &str = "https://dontspeak.org";

/// Product version — the Rust workspace version, shared by every platform's UI so
/// the displayed number has ONE source. Exposed as `ds_version()`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brand colors (hex sRGB) — the single CROSS-PLATFORM source of truth, the visual
/// analogue of the shared `ds-i18n` string catalog: macOS (Swift `Brand`) and Windows
/// (WinUI) both read these via [`ds_brand_colors_json`](crate::ffi) so the same
/// tint renders everywhere instead of each platform hardcoding its own. Keys:
///
/// * `seed_purple` — the icon seed / menu-bar "speaking" pill.
/// * `mic_orange` — the menu-bar "recording" pill (system mic-in-use cue).
/// * `warning` — the warming/blocked/downloading status dots AND the dictation panel's
///   no-focus glow (one orange for "attention/not ready").
pub const BRAND_COLORS_JSON: &str =
    r##"{"seed_purple":"#5B4397","mic_orange":"#FF9F0A","warning":"#FF9F0A"}"##;

/// Logs-tab colors (hex sRGB) — the single CROSS-PLATFORM source of truth for the activity-log
/// view, living beside [`BRAND_COLORS_JSON`] so every platform's Logs tab colors identically
/// instead of hardcoding its own. Read via [`ds_log_colors_json`](crate::ffi). Shape:
///
/// * `levels` — map of the non-ordinary log levels to a color (`ERROR`, `WARN`); `INFO` is
///   intentionally absent (it renders in the default text color).
/// * `source_palette` — an ordered list of distinct, theme-neutral colors; a UI assigns each
///   distinct `source` the palette entry at its FIRST-APPEARANCE index (mod the length) — a
///   convention that's identical on every platform because they read the same ordered lines.
pub const LOG_COLORS_JSON: &str = r##"{"levels":{"ERROR":"#E84646","WARN":"#FF9F0A"},"source_palette":["#8B7BD8","#3FA7A1","#5B8DEF","#4CAF6E","#D97FB0","#CB8A3E","#49B6C2","#B07BD8"]}"##;
