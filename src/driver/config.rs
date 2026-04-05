use std::path::PathBuf;

use clap::ValueEnum;

pub(crate) const DEFAULT_INTERPRETER_TAPE_LEN: usize = 30_000;

#[derive(Debug, Clone)]
pub(crate) struct DriverConfig {
    pub(crate) input: PathBuf,
    pub(crate) source: String,
    pub(crate) mode: RunMode,
    pub(crate) target: CompileTarget,
    pub(crate) output: PathBuf,
    /// When true in `interpret` mode, print tape statistics to stderr after the run.
    pub(crate) interp_debug: bool,
    /// `-O` tier: HIR uses `optimize_o0` / `optimize_o1` / `optimize_o2`; `-O3` additionally enables whole-program `compile` folds.
    pub(crate) opt_level: OptLevel,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// 跑通流水线并在日志中输出各阶段规模，不写 ELF 或 listing
    Dump,
    /// 在优化后的 HIR 上解释执行（默认）
    Interpret,
    /// 生成 target 对应的原生可执行文件，并在 `-o` 旁写出 `.asm` / `.lst`
    Compile,
}

impl RunMode {
    pub(crate) fn as_str(self) -> &'static str {
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

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub(crate) enum CompileTarget {
    /// Handwritten x86_64 Linux ELF backend.
    #[value(name = "x86_64-linux")]
    X86_64Linux,
    /// Handwritten x86_64 Windows PE backend.
    #[value(name = "x86_64-windows")]
    X86_64Windows,
}

impl CompileTarget {
    pub(crate) const fn build_default() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::X86_64Windows
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self::X86_64Linux
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::X86_64Windows => "x86_64-windows",
        }
    }

    pub(crate) const fn default_output_name(self) -> &'static str {
        match self {
            Self::X86_64Linux => "a.out",
            Self::X86_64Windows => "a.exe",
        }
    }
}

impl std::fmt::Display for CompileTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `-O` tier: selects the HIR optimization pass; `-O2` repeats `-O1` to a fixed point; `-O3` additionally enables whole-program folds in `compile` mode (HIR same as `-O2`).
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq, Default)]
pub(crate) enum OptLevel {
    /// HIR: fuse consecutive `Move` / `Add` only (single pass).
    #[default]
    #[value(name = "0")]
    O0,
    /// HIR: one pass of peephole (`Move`/`Add` fusion, `Zero`/`Add` simplification) and
    /// loop simplification (e.g. `[-]` → `Zero`).
    #[value(name = "1")]
    O1,
    /// HIR: repeat `-O1` until a fixed point (peephole / specialization until stable).
    #[value(name = "2")]
    O2,
    /// Strongest available compile optimizations (e.g. fold programs with no `.`,
    /// or precompute stdout when there is no `,`). HIR tier matches `-O2`.
    #[value(name = "3")]
    O3,
}

impl OptLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::O0 => "0",
            Self::O1 => "1",
            Self::O2 => "2",
            Self::O3 => "3",
        }
    }
}

impl std::fmt::Display for OptLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::CompileTarget;

    #[test]
    fn compile_target_default_output_names_match_platform_convention() {
        assert_eq!(CompileTarget::X86_64Linux.default_output_name(), "a.out");
        assert_eq!(CompileTarget::X86_64Windows.default_output_name(), "a.exe");
    }

    #[test]
    fn build_default_tracks_compile_time_target() {
        #[cfg(target_os = "windows")]
        assert_eq!(CompileTarget::build_default(), CompileTarget::X86_64Windows);

        #[cfg(not(target_os = "windows"))]
        assert_eq!(CompileTarget::build_default(), CompileTarget::X86_64Linux);
    }
}
