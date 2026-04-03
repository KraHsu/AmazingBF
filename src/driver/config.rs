use std::path::PathBuf;

use clap::ValueEnum;

#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub input: PathBuf,
    pub source: String,
    pub mode: RunMode,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RunMode {
    /// 跑通流水线并在日志中输出各阶段规模，不写 ELF 或 listing
    Dump,
    /// 在优化后的 HIR 上解释执行（默认）
    Interpret,
    /// 生成 Linux x86_64 ELF，并在 `-o` 旁写出 `.asm` / `.lst`
    Compile,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dump => "dump",
            Self::Interpret => "interpret",
            Self::Compile => "compile",
        }
    }
}

impl std::fmt::Display for RunMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
