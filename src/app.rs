//! Binary startup glue: convert CLI parsing outcomes into process behavior, then run the driver.

use crate::Result;
use crate::cli::{self, CliError};

type ParseFn = fn() -> std::result::Result<cli::AppConfig, CliError>;

/// Run the provided CLI parser, translate [`CliError`] outcomes into appropriate
/// process behaviour (usage / version / help exits), then hand the resulting
/// [`cli::AppConfig`] to `driver::run`.
pub(crate) fn run_with_parse(parse: ParseFn) -> Result<()> {
    let config = match parse() {
        Ok(config) => config,
        Err(CliError::Help(kind)) => {
            cli::print_help(kind);
            std::process::exit(0);
        }
        Err(CliError::Version(kind)) => {
            eprintln!("{} {}", kind.title(), env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Err(CliError::Usage { message, quiet }) => {
            if !quiet {
                eprintln!("{message}");
            }
            std::process::exit(2);
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
        Err(CliError::Io { path, source }) => {
            eprintln!("错误: 无法读取源 `{path}`: {source}");
            std::process::exit(1);
        }
    };

    crate::logging::init_logger(config.log_level)?;
    crate::driver::run::run(config.driver_cfg)
}
