//! AmazingBF library crate.
//!
//! The stable public entry points of this crate are the three `run_*` helpers used by
//! the shipped binaries. Internal pipeline, CLI, IR, runtime, and backend modules stay
//! crate-private so the implementation can evolve without exposing a large semver surface.

#![allow(non_snake_case)] // package name `AmazingBF` is intentional for branding

use anyhow::Result;

mod app;
mod backend;
mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

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
