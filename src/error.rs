//! Unified error type for the library and driver pipeline.

use std::path::Path;

/// Library-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error returned by `run_amazingbf` and related entry points.
#[derive(Debug)]
pub enum Error {
    /// Log verbosity index was not in `0..=4`.
    InvalidLogLevel(u8),
    Io {
        message: String,
        source: std::io::Error,
    },
    Parse(crate::frontend::parser::ParseError),
    Optimize(crate::ir::optimize::OptimizeError),
    Runtime(crate::interp::engine::RuntimeError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidLogLevel(n) => {
                write!(f, "log level must be between 0 and 4, got {n}")
            }
            Error::Io { message, .. } => f.write_str(message),
            Error::Parse(e) => write!(f, "{e}"),
            Error::Optimize(e) => write!(f, "{e}"),
            Error::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Parse(e) => Some(e),
            Error::Optimize(e) => Some(e),
            Error::Runtime(e) => Some(e),
            _ => None,
        }
    }
}

/// Attach a path and verb to an I/O error (replaces `anyhow::Context` for files).
pub(crate) fn io_err(path: &Path, verb: &str, source: std::io::Error) -> Error {
    Error::Io {
        message: format!("failed to {verb} {}", path.display()),
        source,
    }
}

impl From<crate::frontend::parser::ParseError> for Error {
    fn from(value: crate::frontend::parser::ParseError) -> Self {
        Error::Parse(value)
    }
}

impl From<crate::ir::optimize::OptimizeError> for Error {
    fn from(value: crate::ir::optimize::OptimizeError) -> Self {
        Error::Optimize(value)
    }
}

impl From<crate::interp::engine::RuntimeError> for Error {
    fn from(value: crate::interp::engine::RuntimeError) -> Self {
        Error::Runtime(value)
    }
}
