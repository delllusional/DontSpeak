//! Engine logging → the unified activity log.
//!
//! The module is named `logging` (not `log`) so the crate-root re-export
//! `pub(crate) use logging::log` lets call sites — including the sibling modules
//! that pre-date the split — reach the function as `crate::log(...)` without a
//! module/function name collision.

use ds_config::Paths;

/// Cached `Paths` for the unified logger (resolved once; cheap, fail-quiet).
static LOG_PATHS: std::sync::OnceLock<Option<Paths>> = std::sync::OnceLock::new();

/// Engine logging → the unified activity log (`ds_config::log`, source = `engine`).
/// Call sites keep their `"WARN:"` / `"FATAL:"` / `"ERROR:"` message prefixes; we
/// map those to a structured [`ds_config::LogLevel`] and strip the prefix so the
/// stored line carries the level separately. Falls back to stderr (→ launchd) only
/// if `$HOME` can't be resolved.
pub(crate) fn log(s: &str) {
    let (level, msg) = split_level(s);
    match LOG_PATHS.get_or_init(Paths::resolve) {
        Some(p) => ds_config::log(p, level, "engine", msg),
        None => eprintln!("{s}"),
    }
}

/// Map a leading severity word to a [`ds_config::LogLevel`], returning the message
/// with that prefix stripped. Unprefixed lines are `INFO`.
fn split_level(s: &str) -> (ds_config::LogLevel, &str) {
    use ds_config::LogLevel::*;
    for (pfx, lvl) in [("FATAL:", Error), ("ERROR:", Error), ("WARN:", Warn)] {
        if let Some(rest) = s.strip_prefix(pfx) {
            return (lvl, rest.trim_start());
        }
    }
    (Info, s)
}
