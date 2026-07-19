//! Unified activity log (wire + rotation in `log.rs`). Path-based API (no `ds-config`
//! dep — avoids cycle). Logs-tab pure rules: [`parse_logs_json`] / [`filter_logs`] /
//! [`distinct_sources`].
mod catalog;
mod facade;
mod log;
mod log_watch;

pub use catalog::{LogLine, distinct_sources, filter_logs, flatten_log_lines, parse_logs_json};
pub use facade::init;
pub use log::{
    LogLevel, aux_log_path, clear_logs, combined_log_json, log, log_from, log_tail, open_aux_log,
    rotate_if_large,
};
pub use log_watch::wait_logs_changed;
