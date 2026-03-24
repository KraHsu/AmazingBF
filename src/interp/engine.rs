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
