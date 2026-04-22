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
use crate::ir::hir::HirProgram;
use crate::runtime::host::HostRuntime;
use crate::runtime::io::{IoError, RuntimeIo};
use crate::runtime::tape::Tape;

/// Errors raised by the HIR interpreter at runtime.
#[derive(Debug)]
pub enum RuntimeError {
    /// I/O failure while reading or writing Brainfuck cells.
    Io(String),
    /// Reserved for future host-call support in the interpreter.
    #[allow(dead_code)] // reason: constructed once host-call lowering lands
    Host(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Io(msg) => write!(f, "io error: {msg}"),
            RuntimeError::Host(msg) => write!(f, "host error: {msg}"),
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
}

impl<I: RuntimeIo, H: HostRuntime> Interpreter<I, H> {
    /// Create an interpreter with an `tape_len`-byte data tape and the supplied I/O and host runtime.
    pub(crate) fn new(tape_len: usize, io: I, host: H) -> Self {
        Self {
            tape: Tape::new(tape_len),
            io,
            host,
        }
    }

    /// Execute a HIR program to completion, reporting the first I/O or host error encountered.
    pub(crate) fn run(&mut self, program: &HirProgram) -> Result<(), RuntimeError> {
        let bytecode = lower_hir_to_bytecode(program);
        self.exec_bytecode(&bytecode)
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
