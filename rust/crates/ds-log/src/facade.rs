//! `log` crate sink over `log_cached` — use `log::info!`/… instead of calling `ds_log` directly.
//! Does not touch rotation / atomic writes / live watch in `log.rs`.

use crate::log::{LogLevel, log_cached};

/// Pure `log::Record` → `(level, target, message)`, factored for unit tests without
/// `log::set_logger` (once per process). Owned `String`s: `Record`'s `target`/`args` share
/// one lifetime, so borrowing would over-constrain callers (`format_args!` temps die at the
/// enclosing statement).
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

/// Install the unified-log sink as the process global `log` backend (baseline `Info`).
/// Idempotent (engine stop/restart in one host). Raise via `log::set_max_level` when
/// `DONTSPEAK_DEBUG` is set.
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
        // `format_args!` temporary lives only for the enclosing statement.
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
