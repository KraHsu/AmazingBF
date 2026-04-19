//! Unified error type for the library and driver pipeline.

use std::path::Path;

/// Library-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error returned by `run_amazingbf` and related entry points.
#[derive(Debug)]
pub enum Error {
    /// Log verbosity index was not in `0..=4`.
    InvalidLogLevel(u8),
    /// Filesystem / stdio failure with a context-prefixed message.
    Io {
        /// Human-readable description (e.g. `"failed to read foo.bf"`).
        message: String,
        /// Underlying OS error from the standard library.
        source: std::io::Error,
    },
    /// Brainfuck source failed to parse.
    Parse(crate::frontend::parser::ParseError),
    /// HIR optimisation pipeline reported an error.
    Optimize(crate::ir::optimize::OptimizeError),
    /// Interpreter aborted due to a runtime I/O or host-call failure.
    Runtime(crate::interp::engine::RuntimeError),
    /// Catch-all for errors stringified from other subsystems (e.g. `bfsc`).
    Other(String),
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
            Error::Other(s) => f.write_str(s),
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
