use anyhow::Result;

mod backend;
mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

#[cfg(all(feature = "llvm18", feature = "llvm22"))]
compile_error!("features `llvm18` and `llvm22` cannot be enabled at the same time");

fn main() -> Result<()> {
    let config = cli::parse_cli()?;

    driver::run::run(config.driver_cfg)?;

    Ok(())
}
