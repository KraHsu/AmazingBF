use std::path::PathBuf;

use clap::ValueEnum;

#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub source: String,
    pub mode: RunMode,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunMode {
    /// output fontend and IR
    Dump,
    Interpret,
    ToElf,
}
