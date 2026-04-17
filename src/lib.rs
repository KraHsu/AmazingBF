//! AmazingBF library crate.
//!
//! The stable public entry points of this crate are the three `run_*` helpers used by
//! the shipped binaries. Internal pipeline, CLI, IR, runtime, and backend modules stay
//! crate-private so the implementation can evolve without exposing a large semver surface.

#![allow(non_snake_case)] // package name `AmazingBF` is intentional for branding

mod app;
mod backend;
mod bfsc;
mod cli;
mod driver;
pub mod error;
mod frontend;
mod interp;
mod ir;
mod logging;
mod runtime;

pub use error::{Error, Result};

/// Default `AmazingBF` binary: full CLI including `-m` / `--mode`.
pub fn run_amazingbf() -> Result<()> {
    app::run_with_parse(cli::parse_cli)
}

/// `bf-interpreter` binary: fixed interpret mode.
pub fn run_bf_interpreter() -> Result<()> {
    app::run_with_parse(cli::parse_interpreter_cli)
}

/// `bf-compiler` binary: fixed compile mode.
pub fn run_bf_compiler() -> Result<()> {
    app::run_with_parse(cli::parse_compiler_cli)
}

/// `bfsc` binary: BFS (Brainf Script) → BF compiler.
pub fn run_bfsc() -> Result<()> {
    bfsc::run().map_err(|e| crate::error::Error::Other(e.to_string()))
}
