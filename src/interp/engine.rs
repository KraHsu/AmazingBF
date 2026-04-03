use crate::ir::hir::{HirInst, HirProgram};
use crate::runtime::host::HostRuntime;
use crate::runtime::io::{IoError, RuntimeIo};
use crate::runtime::tape::{Tape, TapeError};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("tape error: {0}")]
    Tape(#[from] TapeError),

    #[error("io error: {0}")]
    Io(String),

    #[error("host error: {0}")]
    Host(String),
}

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
pub struct Interpreter<I: RuntimeIo, H: HostRuntime> {
    pub tape: Tape,
    pub io: I,
    pub host: H,
}

/// `(v * f)` reduced to a single `CellAdd`-style delta mod 256 (Brainfuck tape).
fn mul_add_delta_u8(v: u8, f: i32) -> i32 {
    let p = (v as i32).wrapping_mul(f);
    let m = p.rem_euclid(256) as i32;
    if m <= 127 {
        m
    } else {
        m - 256
    }
}

impl<I: RuntimeIo, H: HostRuntime> Interpreter<I, H> {
    pub fn new(tape_len: usize, io: I, host: H) -> Self {
        Self {
            tape: Tape::new(tape_len),
            io,
            host,
        }
    }

    pub fn run(&mut self, program: &HirProgram) -> Result<(), RuntimeError> {
        self.exec_block(&program.insts)
    }

    fn exec_block(&mut self, insts: &[HirInst]) -> Result<(), RuntimeError> {
        for inst in insts {
            match inst {
                HirInst::Move(delta) => {
                    self.tape.move_ptr(*delta)?;
                }
                HirInst::Add(delta) => {
                    self.tape.add_current(*delta);
                }
                HirInst::PutByte => {
                    let byte = self.tape.current();
                    self.io.put_byte(byte)?;
                }
                HirInst::GetByte => {
                    let byte = self.io.get_byte()?;
                    self.tape.set_current(byte);
                }
                HirInst::Zero => {
                    self.tape.set_current(0);
                }
                HirInst::LinearMul(factors) => {
                    let v = self.tape.current();
                    self.tape.set_current(0);
                    for (off, f) in factors {
                        self.tape.move_ptr(*off)?;
                        let delta = mul_add_delta_u8(v, *f);
                        self.tape.add_current(delta);
                        self.tape.move_ptr(-*off)?;
                    }
                }
                HirInst::Scan(dir) => {
                    let step = match *dir {
                        1 | -1 => *dir,
                        _ => dir.signum(),
                    };
                    while self.tape.current() != 0 {
                        self.tape.move_ptr(step)?;
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
