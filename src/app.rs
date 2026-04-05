//! Binary startup glue: convert CLI parsing outcomes into process behavior, then run the driver.

use anyhow::Result;

use crate::cli::{self, CliError};

type ParseFn = fn() -> Result<cli::AppConfig, CliError>;

pub(crate) fn run_with_parse(parse: ParseFn) -> Result<()> {
    let config = match parse() {
        Ok(config) => config,
        Err(CliError::Clap { err, quiet }) => {
            if !quiet {
                let _ = err.print();
            }
            std::process::exit(match err.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
                _ => 2,
            });
        }
        Err(CliError::Message {
            message,
            quiet,
            exit_code,
        }) => {
            if !quiet {
                eprintln!("{message}");
            }
            std::process::exit(exit_code);
        }
        Err(CliError::Other(err)) => return Err(err),
    };

    crate::driver::logging::init_logger(config.log_level)?;
    crate::driver::run::run(config.driver_cfg)
}
