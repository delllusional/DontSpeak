//! Hermes Agent usage — stub (no public quota API yet).

use crate::UsageRow;

/// No documented usage endpoint; fail closed with NotFound so the cache keeps last-good.
pub(crate) fn fetch(_paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Hermes Agent has no usage API integration yet",
    ))
}
