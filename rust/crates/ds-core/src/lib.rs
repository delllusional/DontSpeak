//! UI-agnostic core of DontSpeak: engine liveness ([`engine`]), model presence
//! ([`models`]), model-status JSON, in-process engine lifecycle, and locale/provider
//! controls over a small C ABI ([`ffi`], committed `dontspeak.h`). Rich config/voice
//! control lives in DontSpeak. `ds_update_check_json` is the only network FFI call;
//! everything else is disk/IPC.
//!
//! - macOS: SwiftUI staticlib (`apps/macos/`)
//! - Linux: GTK staticlib (`ds-gtk`); Windows: WinUI cdylib (`ds_core.dll`)

pub mod engine;
pub mod ffi;
pub(crate) mod host;
pub mod models;
pub mod status_fmt;

/// Product homepage — single source for every platform UI (`ds_homepage_url`).
pub const HOMEPAGE_URL: &str = "https://dontspeak.org";

/// Workspace version shared by every platform UI (`ds_version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brand colors (hex sRGB) — cross-platform source of truth (visual analogue of
/// `ds-i18n`). Read via [`ds_brand_colors_json`](crate::ffi).
///
/// * `seed_purple` — icon seed / menu-bar "speaking" pill
/// * `mic_orange` — menu-bar "recording" pill
/// * `warning` — warming/blocked/downloading dots + dictation no-focus glow
pub const BRAND_COLORS_JSON: &str =
    r##"{"seed_purple":"#5B4397","mic_orange":"#FF9F0A","warning":"#FF9F0A"}"##;

/// Logs-tab colors (hex sRGB) next to [`BRAND_COLORS_JSON`]. Via
/// [`ds_log_colors_json`](crate::ffi).
///
/// * `levels` — `ERROR`/`WARN` only (`INFO` uses default text color)
/// * `source_palette` — ordered theme-neutral colors; each distinct `source` gets
///   first-appearance index mod length (same convention every host)
pub const LOG_COLORS_JSON: &str = r##"{"levels":{"ERROR":"#E84646","WARN":"#FF9F0A"},"source_palette":["#8B7BD8","#3FA7A1","#5B8DEF","#4CAF6E","#D97FB0","#CB8A3E","#49B6C2","#B07BD8"]}"##;
