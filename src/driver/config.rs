use std::path::PathBuf;

use clap::ValueEnum;

#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub input: PathBuf,
    pub source: String,
    pub mode: RunMode,
    pub output: PathBuf,
    /// When true in `interpret` mode, print tape statistics to stderr after the run.
    pub interp_debug: bool,
    /// `-O` tier: HIR pipeline uses `optimize_o0` vs `optimize_o1`; `-O3` additionally enables whole-program `compile` folds.
    pub opt_level: OptLevel,
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

/// `-O` tier: selects the HIR optimization pass; `-O3` additionally enables whole-program folds in `compile` mode.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// HIR: fuse consecutive `Move` / `Add` only (single pass).
    #[default]
    #[value(name = "0")]
    O0,
    /// HIR: one pass of peephole (`Move`/`Add` fusion, `Zero`/`Add` simplification) and
    /// loop simplification (e.g. `[-]` → `Zero`).
    #[value(name = "1")]
    O1,
    /// Strongest available compile optimizations (e.g. fold programs with no `.`,
    /// or precompute stdout when there is no `,`).
    #[value(name = "3")]
    O3,
}

impl OptLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::O0 => "0",
            Self::O1 => "1",
            Self::O3 => "3",
        }
    }
}

impl std::fmt::Display for OptLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
