//! HIR interpreter engine.
//!
//! Runs a Brainfuck program against a shared [`Tape`] and `RuntimeIo`. The
//! engine does *not* walk HIR directly: it first lowers to the
//! superinstruction form in [`crate::interp::bytecode::InterpOp`] via
//! [`crate::interp::lower::lower_hir_to_bytecode`], then dispatches a flat
//! `pc`-indexed loop over that stream. `[` / `]` become direct jumps
//! (`LoopStart` / `LoopEnd` carry absolute pc targets), so loop iteration
//! costs one branch per cell test instead of a Rust recursive call per
//! iteration.
//!
//! The lowering happens once per `run()` call; driver code that invokes the
//! interpreter twice (e.g. -O3's precomputed-stdout path) pays the lowering
//! cost twice, which is still negligible compared with the execution.

use crate::interp::bytecode::InterpProgram;
use crate::interp::handlers::dispatch_table;
use crate::interp::lower::lower_hir_to_bytecode;
use crate::interp::profile::LoopProfile;
use crate::ir::hir::HirProgram;
use crate::runtime::host::HostRuntime;
use crate::runtime::io::{IoError, RuntimeIo};
use crate::runtime::tape::Tape;
use std::sync::Arc;

/// State of the tiered-JIT slot for a single loop, keyed by `LoopStart` pc.
///
/// Stored in a flat `Vec<JitState>` indexed by pc rather than a `HashMap`
/// so the back-edge fast path can do a single bounds-checked load + tag
/// compare per iteration. Bench (mandelbrot.b -O3 tiered, 2026-04-28) showed
/// the prior `HashMap::contains_key` lookup added ~25 s to a 7.8 s run when
/// every hot loop was `Failed` — a 4× regression vs. plain interpret.
///
/// `Failed` is sticky once an eligibility analysis or machine-code emission
/// rejects the loop, so the back-edge short-circuits without retrying.
/// `Ready` carries the live `JitBuffer` plus the `(min_off, max_off)` reach
/// derived during eligibility analysis, used to pre-grow the tape before
/// each call.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub(crate) enum JitState {
    /// Loop has not yet crossed the trip-count threshold.
    #[default]
    Cold,
    /// Eligibility analysis or machine-code emission rejected this loop;
    /// never retried.
    Failed,
    /// Loop has compiled successfully; dispatch this on the next back-edge.
    Ready {
        buf: amazingbf_jit::JitBuffer,
        reach: (i32, i32),
    },
}

/// Errors raised by the HIR interpreter at runtime.
#[derive(Debug)]
pub enum RuntimeError {
    /// I/O failure while reading or writing Brainfuck cells.
    Io(String),
    /// Reserved for future host-call support in the interpreter.
    #[allow(dead_code)] // reason: constructed once host-call lowering lands
    Host(String),
    /// Tiered-JIT execution returned a non-zero exit code, or a JIT-side
    /// allocation (mmap of the bridging tape) failed.
    #[cfg(target_os = "linux")]
    Jit(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Io(msg) => write!(f, "io error: {msg}"),
            RuntimeError::Host(msg) => write!(f, "host error: {msg}"),
            #[cfg(target_os = "linux")]
            RuntimeError::Jit(msg) => write!(f, "jit error: {msg}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<IoError> for RuntimeError {
    fn from(err: IoError) -> Self {
        match err {
            IoError::ReadError(msg) => RuntimeError::Io(msg),
            IoError::WriteError(msg) => RuntimeError::Io(msg),
        }
    }
}

/// The interpreter execution engine.
///
/// It depends on three runtime components:
/// - `Tape`: the memory tape
/// - `RuntimeIo`: input and output
/// - `HostRuntime`: host extension calls
pub(crate) struct Interpreter<I: RuntimeIo, H: HostRuntime> {
    /// Data tape (auto-growing right-side).
    pub(crate) tape: Tape,
    /// `RuntimeIo` backend used for `,` and `.` instructions.
    pub(crate) io: I,
    /// Host-call runtime; unused until host-call lowering reaches the interpreter.
    #[allow(dead_code)] // reason: used once host-call lowering reaches the interpreter
    pub(crate) host: H,
    /// Optional loop trip-count profiler (F1b foundation).
    pub(crate) profile: Option<LoopProfile>,
    /// The bytecode currently executing. Stored as an `Arc` so handlers
    /// can look up loop bodies (e.g. for hot-loop JIT compilation) without
    /// fighting the `&mut self` borrow on the dispatch loop.
    pub(crate) program: Option<Arc<InterpProgram>>,
    /// Whether the F1b tiered JIT is enabled. When true, `handle_loop_end`
    /// triggers compilation + dispatch on hot back-edges.
    #[cfg(target_os = "linux")]
    pub(crate) jit_enabled: bool,
    /// Per-pc tiered-JIT state, indexed by `LoopStart` pc. Sized to the
    /// bytecode `ops.len()` in `run()` so non-LoopStart slots stay `Cold`
    /// (and are never read). Indexed access keeps the back-edge fast path
    /// to a single load + tag compare per iteration.
    #[cfg(target_os = "linux")]
    pub(crate) jit_cache: Vec<JitState>,
}

impl<I: RuntimeIo, H: HostRuntime> Interpreter<I, H> {
    /// Create an interpreter with an `tape_len`-byte data tape and the supplied I/O and host runtime.
    pub(crate) fn new(tape_len: usize, io: I, host: H) -> Self {
        Self {
            tape: Tape::new(tape_len),
            io,
            host,
            profile: None,
            program: None,
            #[cfg(target_os = "linux")]
            jit_enabled: false,
            #[cfg(target_os = "linux")]
            jit_cache: Vec::new(),
        }
    }

    /// Enable loop trip-count profiling with the given hot threshold.
    /// Must be called before `run()`.
    #[allow(dead_code)] // reason: still useful for measurement-only runs without JIT
    pub(crate) fn enable_profiling(&mut self, threshold: u64) {
        self.profile = Some(LoopProfile::new(0, threshold));
    }

    /// Enable the F1b tiered JIT: profile loop trip counts and dispatch
    /// JIT-compiled machine code for any loop whose count crosses
    /// `threshold`. Must be called before `run()`. Linux x86_64 only.
    #[cfg(target_os = "linux")]
    pub(crate) fn enable_tiered_jit(&mut self, threshold: u64) {
        self.profile = Some(LoopProfile::new(0, threshold));
        self.jit_enabled = true;
    }

    /// Execute a HIR program to completion, reporting the first I/O or host error encountered.
    ///
    /// After dispatch ends, `io.flush()` is called so buffered adaptors
    /// (e.g. `BufferedStdIo`) can surface a late-flush `IoError` as a
    /// `RuntimeError::Io` instead of silently dropping it. A successful
    /// exec followed by a flush failure is reported as the flush error;
    /// an exec error wins over a flush error.
    pub(crate) fn run(&mut self, program: &HirProgram) -> Result<(), RuntimeError> {
        let bytecode = lower_hir_to_bytecode(program);
        if self.profile.is_some() {
            let threshold = self.profile.as_ref().unwrap().threshold();
            self.profile = Some(LoopProfile::new(bytecode.ops.len(), threshold));
        }
        #[cfg(target_os = "linux")]
        if self.jit_enabled {
            // Resize-with-default to allocate one `JitState::Cold` per op.
            // Non-LoopStart slots are never read; we trade a few bytes of
            // unused state for branch-free indexing in `handle_loop_end`.
            self.jit_cache.clear();
            self.jit_cache.resize_with(bytecode.ops.len(), JitState::default);
        }
        let bytecode = Arc::new(bytecode);
        self.program = Some(bytecode.clone());
        let exec_result = self.exec_bytecode(&bytecode);
        let flush_result = self.io.flush();
        exec_result?;
        flush_result.map_err(RuntimeError::from)
    }

    /// Returns the loop profile after execution, if profiling was enabled.
    #[allow(dead_code)] // reason: surface for measurement consumers (loop_profile bench)
    pub(crate) fn profile(&self) -> Option<&LoopProfile> {
        self.profile.as_ref()
    }

    fn exec_bytecode(&mut self, program: &InterpProgram) -> Result<(), RuntimeError> {
        // Table dispatch: index by `InterpOp::tag()`, then one indirect call
        // per op. The table is a stack-local array of fn pointers — the hot
        // loop pays one load + one indirect branch per op vs. LLVM's
        // monolithic `match` jump table, giving the CPU's indirect-branch
        // predictor more per-opcode state to learn from.
        let table = dispatch_table::<I, H>();
        let ops = program.ops.as_slice();
        let mut pc: usize = 0;
        while pc < ops.len() {
            let op = &ops[pc];
            let handler = table[op.tag()];
            pc = handler(self, op, pc)?;
        }
        Ok(())
    }
}
