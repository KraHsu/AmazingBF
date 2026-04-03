use anyhow::Result;

mod backend;
mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

fn main() -> Result<()> {
    let config = cli::parse_cli()?;

    driver::logging::init_logger(config.log_level)?;

    driver::run::run(config.driver_cfg)?;

    Ok(())
}
