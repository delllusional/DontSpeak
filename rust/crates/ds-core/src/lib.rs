//! UI-agnostic core: liveness, model status, lifecycle + locale via C ABI
//! ([`ffi`], `dontspeak.h`). Only network FFI: `ds_update_check_json`.

pub mod engine;
pub mod ffi;
pub(crate) mod host;
pub mod pastel;
pub mod status_fmt;

/// Homepage — single source for every platform UI (`ds_homepage_url`).
pub const HOMEPAGE_URL: &str = "https://dontspeak.org";

/// Workspace version (`ds_version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brand hex sRGB (visual analogue of `ds-i18n`); via `ds_brand_colors_json`.
/// seed_purple / mic_orange / warning (dots + no-focus glow).
pub const BRAND_COLORS_JSON: &str =
    r##"{"seed_purple":"#5B4397","mic_orange":"#FF9F0A","warning":"#FF9F0A"}"##;

/// Logs-tab colors; via `ds_log_colors_json`.
/// levels: ERROR/WARN; source_palette: first-appearance index mod len.
pub const LOG_COLORS_JSON: &str = r##"{"levels":{"ERROR":"#E84646","WARN":"#FF9F0A"},"source_palette":["#8B7BD8","#3FA7A1","#5B8DEF","#4CAF6E","#D97FB0","#CB8A3E","#49B6C2","#B07BD8"]}"##;
