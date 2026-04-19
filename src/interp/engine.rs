//! HIR interpreter engine.
//!
//! Walks a `HirProgram` against a shared [`Tape`] and `RuntimeIo`, terminating
//! when the instruction list is exhausted. Loop bodies recurse synchronously,
//! so no explicit call stack exists beyond the host stack. This is the final
//! stage of the pipeline in `RunMode::Interpret`.

use crate::ir::hir::{HirInst, HirProgram};
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

/// `(v * f)` reduced to a single `CellAdd`-style delta mod 256 (Brainfuck tape).
fn mul_add_delta_u8(v: u8, f: i32) -> i32 {
    let p = (v as i32).wrapping_mul(f);
    let m = p.rem_euclid(256);
    if m <= 127 { m } else { m - 256 }
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
        self.exec_block(&program.insts)
    }

    fn exec_block(&mut self, insts: &[HirInst]) -> Result<(), RuntimeError> {
        for inst in insts {
            match inst {
                HirInst::Move(delta) => {
                    self.tape.move_ptr(*delta);
                }
                HirInst::Add(delta) => {
                    self.tape.add_current(*delta);
                }
                HirInst::PutByte => {
                    let ptr = self.tape.ptr();
                    let byte = self.tape.current();
                    self.io.put_byte(ptr, byte)?;
                }
                HirInst::GetByte => {
                    let ptr = self.tape.ptr();
                    let byte = self.io.get_byte(ptr)?;
                    self.tape.set_current(byte);
                }
                HirInst::Zero => {
                    self.tape.set_current(0);
                }
                HirInst::LinearMul(factors) => {
                    let v = self.tape.current();
                    self.tape.set_current(0);
                    for (off, f) in factors {
                        self.tape.move_ptr(*off);
                        let delta = mul_add_delta_u8(v, *f);
                        self.tape.add_current(delta);
                        self.tape.move_ptr(-*off);
                    }
                }
                HirInst::Scan(dir) => {
                    let step = match *dir {
                        1 | -1 => *dir,
                        _ => dir.signum(),
                    };
                    while self.tape.current() != 0 {
                        self.tape.move_ptr(step);
                    }
                }
                HirInst::Loop(body) => {
                    while self.tape.current() != 0 {
                        self.exec_block(body)?;
                    }
                }
            }
        }

        Ok(())
    }
}
