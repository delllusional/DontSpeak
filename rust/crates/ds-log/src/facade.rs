//! Implements the `log` crate's `Log` trait as a thin sink over `log_cached`, so callers
//! use `log::info!`/`warn!`/`error!`/`debug!` instead of calling `ds_log::log_cached`
//! directly. Does not touch `log.rs`'s engine (rotation, atomic writes, live watch).

use crate::log::{LogLevel, log_cached};

/// Pure `log::Record` → `(level, target, message)` mapping, factored out so it can be unit
/// tested directly (constructing a real `log::Record`) without going through the global
/// `log::set_logger`, which can only succeed once per process. Returns owned `String`s
/// rather than borrowing from `record`: `log::Record`'s `target` and `args` fields share
/// one lifetime parameter, so a borrowed return would tie `target` to `args`'s lifetime
/// too — needlessly restrictive for callers (and awkward to construct in tests, since
/// `format_args!`'s backing temporary only lives for the enclosing statement).
fn map_record(record: &log::Record) -> (LogLevel, String, String) {
    let level = match record.level() {
        log::Level::Error => LogLevel::Error,
        log::Level::Warn => LogLevel::Warn,
        log::Level::Info => LogLevel::Info,
        log::Level::Debug | log::Level::Trace => LogLevel::Debug,
    };
    (
        level,
        record.target().to_string(),
        record.args().to_string(),
    )
}

struct DsLogLogger;

impl log::Log for DsLogLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let (level, target, message) = map_record(record);
        log_cached(level, &target, &message);
    }
    fn flush(&self) {}
}

static LOGGER: DsLogLogger = DsLogLogger;

/// Install the unified-log sink as this process's global `log` backend, at a baseline
/// max level of `Info`. Idempotent (safe to call more than once per process — e.g.
/// dontspeakd's engine can stop/restart within one long-lived host app; only the first
/// call installs the logger). Callers with their own DONTSPEAK_DEBUG gate raise the level
/// afterward via `log::set_max_level(log::LevelFilter::Debug)`.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Info);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_record_maps_level_target_and_message() {
        let mb = 12;
        // `format_args!`'s backing temporary only lives for the enclosing statement, so
        // build the record and call map_record within a single statement.
        let (level, target, message) = map_record(
            &log::Record::builder()
                .level(log::Level::Warn)
                .target("engine")
                .args(format_args!("disk low: {mb} MB"))
                .build(),
        );
        assert_eq!(level, LogLevel::Warn);
        assert_eq!(target, "engine");
        assert_eq!(message, "disk low: 12 MB");
    }

    #[test]
    fn map_record_collapses_debug_and_trace_to_debug() {
        for lvl in [log::Level::Debug, log::Level::Trace] {
            let (level, _, _) = map_record(
                &log::Record::builder()
                    .level(lvl)
                    .target("x")
                    .args(format_args!("m"))
                    .build(),
            );
            assert_eq!(level, LogLevel::Debug);
        }
    }

    #[test]
    fn map_record_maps_error_and_info() {
        let (error_level, _, _) = map_record(
            &log::Record::builder()
                .level(log::Level::Error)
                .target("t")
                .args(format_args!("e"))
                .build(),
        );
        assert_eq!(error_level, LogLevel::Error);

        let (info_level, _, _) = map_record(
            &log::Record::builder()
                .level(log::Level::Info)
                .target("t")
                .args(format_args!("i"))
                .build(),
        );
        assert_eq!(info_level, LogLevel::Info);
    }
}
