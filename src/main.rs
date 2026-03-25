use anyhow::Result;
use tracing::{Level, info, instrument, warn};
use tracing_subscriber::{EnvFilter, fmt};

mod backend;
mod cli;
mod driver;
mod frontend;
mod interp;
mod ir;
mod runtime;

#[cfg(all(feature = "llvm18", feature = "llvm22"))]
compile_error!("features `llvm18` and `llvm22` cannot be enabled at the same time");

fn init_logger(log_level: u8) {
    if log_level == 0 {
        return;
    }

    let crate_name = env!("CARGO_PKG_NAME").replace('-', "_");

    let filter = match log_level {
        1 => EnvFilter::new(format!("warn,{crate_name}=info")),
        2 => EnvFilter::new(format!("warn,{crate_name}=debug")),
        3 => EnvFilter::new(format!("info,{crate_name}=debug")),
        4 => EnvFilter::new("debug"),
        _ => {
            eprintln!("log level must be between 0 and 4");
            std::process::exit(1);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or(filter))
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .init();
}

fn main() -> Result<()> {
    let config = cli::parse_cli()?;

    init_logger(config.log_level);

    driver::run::run(config.driver_cfg)?;

    Ok(())
}
