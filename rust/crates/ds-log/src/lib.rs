//! The workspace-wide unified activity log — see `log.rs`'s module doc for the wire
//! format and rotation scheme. Split out of `ds-config` (issue #6): this module's only
//! real dependency was `Paths::log_file`, so its public API takes a plain `&Path`
//! instead of `ds_config::Paths` — giving this crate ZERO dependency on `ds-config`,
//! which lets `ds-config` depend on THIS crate for its own internal diagnostics
//! (`VoiceConfig::load`'s "config.toml is not valid TOML" warning) without a cycle.
mod facade;
mod log;
mod log_watch;

pub use facade::init;
pub use log::{
    LogLevel, aux_log_path, clear_logs, combined_log_json, log, log_cached, log_from, log_tail,
    open_aux_log, rotate_if_large,
};
pub use log_watch::wait_logs_changed;
