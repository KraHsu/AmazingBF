//! Minimal stderr logging: verbosity from CLI (`-q` / `-v`), no external crates.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::error::Error;
use crate::error::Result;

static LOG_LEVEL: AtomicU8 = AtomicU8::new(0);

fn active_level() -> u8 {
    LOG_LEVEL.load(Ordering::Relaxed)
}

/// Initialise the process-wide log verbosity. Returns [`Error::InvalidLogLevel`]
/// when `log_level` is outside `0..=4`.
pub(crate) fn init_logger(log_level: u8) -> Result<()> {
    if log_level > 4 {
        return Err(Error::InvalidLogLevel(log_level));
    }
    LOG_LEVEL.store(log_level, Ordering::Relaxed);
    Ok(())
}

/// User-facing progress lines (default verbosity and up).
pub(crate) fn log_info(msg: impl AsRef<str>) {
    if active_level() >= 1 {
        eprintln!("{}", msg.as_ref());
    }
}

/// Detailed pipeline diagnostics (`-v` and up).
pub(crate) fn log_debug(msg: impl AsRef<str>) {
    if active_level() >= 2 {
        eprintln!("{}", msg.as_ref());
    }
}
