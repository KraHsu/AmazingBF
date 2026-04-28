//! Compile-time configuration assembled from CLI flags.
//!
//! `DriverConfig` is the single source of truth passed from `cli` to
//! `driver::run`. `RunMode`, `CompileTarget`, and `OptLevel` are parsed and
//! normalised here; downstream layers assume the resulting config is fully
//! validated (e.g. `output` is populated with a platform-appropriate default
//! when the user did not pass `-o`).

use std::path::PathBuf;

/// Default Brainfuck tape length used by the interpreter when no override is set.
pub(crate) const DEFAULT_INTERPRETER_TAPE_LEN: usize = 30_000;

/// Default hot-loop trip-count threshold for `-m tiered` when `--jit-threshold`
/// is not supplied. Set high enough that micro-loops in program startup stay in
/// the interpreter, but low enough that genuinely hot loops cross within the
/// first few milliseconds of work (hanoi.b / long.b's hot loops fire 10⁸+ times).
#[cfg(target_os = "linux")]
pub(crate) const DEFAULT_JIT_THRESHOLD: u64 = 10_000;

/// Normalised compile-time configuration consumed by `driver::run`.
#[derive(Debug, Clone)]
pub(crate) struct DriverConfig {
    /// Path to the input Brainfuck source file.
    pub(crate) source: String,
    /// Which pipeline stage to run (interpret / dump / compile).
    pub(crate) mode: RunMode,
    /// Target triple for native code generation in `compile` mode.
    pub(crate) target: CompileTarget,
    /// Destination path for compile-mode artefacts (`.out` / `.exe`).
    pub(crate) output: PathBuf,
    /// When true in `interpret` mode, print tape statistics to stderr after the run.
    pub(crate) interp_debug: bool,
    /// `-O` tier: HIR uses `optimize_o0` / `optimize_o1` / `optimize_o2`; `-O3` additionally enables whole-program `compile` folds.
    pub(crate) opt_level: OptLevel,
    /// Hot-loop trip-count threshold for `RunMode::Tiered`. Ignored in
    /// other modes. `None` keeps the built-in default (`DEFAULT_JIT_THRESHOLD`).
    #[cfg(target_os = "linux")]
    pub(crate) jit_threshold: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Run the full pipeline and log per-stage sizes; do not write ELF / listings.
    Dump,
    /// Interpret the optimised HIR (default).
    Interpret,
    /// Emit a native executable for `target` plus `.asm` / `.lst` alongside `-o`.
    Compile,
    /// JIT-compile and execute in-process (Linux x86_64 only).
    #[cfg(target_os = "linux")]
    Jit,
    /// Tiered JIT: interpret with hot-loop profiling, then dispatch JIT-compiled
    /// machine code for any loop whose trip count crosses the threshold (Linux
    /// x86_64 only).
    #[cfg(target_os = "linux")]
    Tiered,
}

impl RunMode {
    /// Parse the `--mode` CLI flag; returns `None` for unknown values.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "dump" => Some(Self::Dump),
            "interpret" => Some(Self::Interpret),
            "compile" => Some(Self::Compile),
            #[cfg(target_os = "linux")]
            "jit" => Some(Self::Jit),
            #[cfg(target_os = "linux")]
            "tiered" => Some(Self::Tiered),
            _ => None,
        }
    }

    /// Canonical CLI name for this mode.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Dump => "dump",
            Self::Interpret => "interpret",
            Self::Compile => "compile",
            #[cfg(target_os = "linux")]
            Self::Jit => "jit",
            #[cfg(target_os = "linux")]
            Self::Tiered => "tiered",
        }
    }
}

impl std::fmt::Display for RunMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompileTarget {
    /// Handwritten x86_64 Linux ELF backend.
    X86_64Linux,
    /// Handwritten x86_64 Windows PE backend.
    X86_64Windows,
}

impl CompileTarget {
    /// Parse the `--target` CLI flag; returns `None` for unknown values.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "x86_64-linux" => Some(Self::X86_64Linux),
            "x86_64-windows" => Some(Self::X86_64Windows),
            _ => None,
        }
    }

    /// Select the default target for the host OS at build time.
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

    /// Canonical CLI name for this target.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::X86_64Windows => "x86_64-windows",
        }
    }

    /// Platform-appropriate default output filename when `-o` is omitted.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OptLevel {
    /// HIR: fuse consecutive `Move` / `Add` only (single pass).
    #[default]
    O0,
    /// HIR: one pass of peephole (`Move`/`Add` fusion, `Zero`/`Add` simplification) and
    /// loop simplification (e.g. `[-]` → `Zero`).
    O1,
    /// HIR: repeat `-O1` until a fixed point (peephole / specialization until stable).
    O2,
    /// Strongest available compile optimizations (e.g. fold programs with no `.`,
    /// or precompute stdout when there is no `,`). HIR tier matches `-O2`.
    O3,
}

impl OptLevel {
    /// Parse the `-O` CLI flag; returns `None` for unknown values.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "0" => Some(Self::O0),
            "1" => Some(Self::O1),
            "2" => Some(Self::O2),
            "3" => Some(Self::O3),
            _ => None,
        }
    }

    /// Canonical CLI digit for this optimisation tier (e.g. `"2"`).
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
