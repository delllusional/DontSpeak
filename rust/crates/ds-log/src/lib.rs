//! Workspace unified activity log — wire format and rotation live in `log.rs`.
//!
//! Split from `ds-config` (issue #6): API takes `&Path` instead of `ds_config::Paths`,
//! so this crate has zero dep on `ds-config` and `ds-config` can depend here for
//! `VoiceConfig::load` diagnostics without a cycle.
mod facade;
mod log;
mod log_watch;

pub use facade::init;
pub use log::{
    LogLevel, aux_log_path, clear_logs, combined_log_json, log, log_from, log_tail, open_aux_log,
    rotate_if_large,
};
pub use log_watch::wait_logs_changed;
