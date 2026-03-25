use crate::driver::config::{DriverConfig, RunMode};

use anyhow::Result;
use clap::Parser;
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

    /// Verbose mode
    #[arg(short, long)]
    verbose: bool,

    /// Run mode
    #[arg(short, long, value_enum, default_value_t = RunMode::Interpret)]
    mode: RunMode,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub driver_cfg: DriverConfig,
    pub verbose: bool,
}

pub fn parse_cli() -> Result<AppConfig> {
    let args = Args::parse();

    let input_file = args.input;

    let source = std::fs::read_to_string(input_file)?;

    let mode = args.mode;

    let output = args.output;

    Ok(AppConfig {
        driver_cfg: DriverConfig {
            source,
            mode,
            output,
        },
        verbose: args.verbose,
    })
}
