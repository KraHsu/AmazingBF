use crate::driver::config::{DriverConfig, RunMode};

use anyhow::Result;
use clap::{ArgAction, Parser, error::ErrorKind};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "A simple bf compiler & interpreter.")]
struct Args {
    /// input file
    #[arg(short, long)]
    input: PathBuf,

    /// output file
    #[arg(short, long, default_value = "a.out")]
    output: PathBuf,

    /// Verbose level (-v, -vv, -vvv)
    #[arg(short, long, action = ArgAction::Count, group = "log_level")]
    verbose: u8,

    /// Quiet log
    #[arg(short, long, action = ArgAction::SetTrue, group = "log_level")]
    quiet: bool,

    /// Run mode
    #[arg(short, long, value_enum, default_value_t = RunMode::Interpret)]
    mode: RunMode,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub driver_cfg: DriverConfig,

    /// 0 -> quiet
    /// 1 -> normal
    /// 2 -> v
    /// 3 -> vv
    /// _ -> vvv
    pub log_level: u8,
}

pub fn parse_cli() -> Result<AppConfig> {
    let quiet_requested = std::env::args_os().any(|arg| arg == "-q" || arg == "--quiet");

    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(err) => {
            if !quiet_requested {
                err.print()?;
            }

            match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    std::process::exit(0);
                }
                _ => {
                    std::process::exit(2);
                }
            }
        }
    };

    let source = std::fs::read_to_string(&args.input)?;

    if args.verbose > 3 {
        if !args.quiet {
            eprintln!("verbose level must be at most 3");
        }
        std::process::exit(1);
    }

    Ok(AppConfig {
        driver_cfg: DriverConfig {
            source,
            mode: args.mode,
            output: args.output,
        },
        log_level: if args.quiet { 0 } else { args.verbose + 1 },
    })
}
