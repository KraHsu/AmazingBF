//! AmazingBF library crate: shared pipeline and CLI helpers for multiple binaries.

#![allow(non_snake_case)] // package name `AmazingBF` is intentional for branding

use anyhow::Result;

mod backend;
mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

fn run_with_parse(parse: fn() -> Result<cli::AppConfig>) -> Result<()> {
    let config = parse()?;
    driver::logging::init_logger(config.log_level)?;
    driver::run::run(config.driver_cfg)?;
    Ok(())
}

/// Default `AmazingBF` binary: full CLI including `-m` / `--mode`.
pub fn run_amazingbf() -> Result<()> {
    run_with_parse(cli::parse_cli)
}

/// `bf-interpreter` binary: fixed interpret mode.
pub fn run_bf_interpreter() -> Result<()> {
    run_with_parse(cli::parse_interpreter_cli)
}

/// `bf-compiler` binary: fixed compile mode.
pub fn run_bf_compiler() -> Result<()> {
    run_with_parse(cli::parse_compiler_cli)
}
