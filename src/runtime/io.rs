//! Byte-oriented runtime I/O shared by the interpreter and compile-time folds.
//!
//! `RuntimeIo` is the abstract byte sink/source; `StdIo` wires it to the
//! process's stdio, while `BufferOutputIo` buffers output into memory (used by
//! `-O3` stdout pre-folding and by tests). The `ptr` argument lets
//! implementations dispatch based on tape address, enabling mmap-style side
//! channels such as the GUI screen buffer.

use std::io::{Read, Write};

/// Byte returned by `get_byte` on stdin EOF, matching the Brainfuck convention
/// shared by the interpreter and the native backend.
const EOF_BYTE: u8 = 255;

/// Error raised by a [`RuntimeIo`] implementation.
#[derive(Debug)]
pub(crate) enum IoError {
    /// Failure while reading a byte (wraps the OS error message).
    ReadError(String),
    /// Failure while writing a byte (wraps the OS error message).
    WriteError(String),
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IoError::ReadError(msg) => write!(f, "read error: {msg}"),
            IoError::WriteError(msg) => write!(f, "write error: {msg}"),
        }
    }
}

impl std::error::Error for IoError {}

/// Byte-oriented runtime IO used by the interpreter and compile-time folds.
///
/// `ptr` is the current tape pointer at the time of the call, allowing
/// implementations to route I/O based on memory address (e.g. screen buffer).
pub(crate) trait RuntimeIo {
    /// Write `byte` to the sink; `ptr` is the current tape pointer at call time.
    fn put_byte(&mut self, ptr: isize, byte: u8) -> Result<(), IoError>;
    /// Read one byte from the source; `ptr` is the current tape pointer at call time.
    fn get_byte(&mut self, ptr: isize) -> Result<u8, IoError>;
}

/// [`RuntimeIo`] backed by the process's stdin / stdout.
pub(crate) struct StdIo;

impl StdIo {
    /// Create a new stdio adaptor; held by value, carries no state.
    pub(crate) fn new() -> Self {
        Self
    }
}

impl RuntimeIo for StdIo {
    fn put_byte(&mut self, _ptr: isize, byte: u8) -> Result<(), IoError> {
        let mut out = std::io::stdout();
        out.write_all(&[byte])
            .map_err(|e| IoError::WriteError(e.to_string()))?;
        out.flush()
            .map_err(|e| IoError::WriteError(e.to_string()))?;
        Ok(())
    }

    fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
        let mut input = std::io::stdin();
        let mut buf = [0u8; 1];

        match input.read_exact(&mut buf) {
            Ok(()) => Ok(buf[0]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(EOF_BYTE),
            Err(e) => Err(IoError::ReadError(e.to_string())),
        }
    }
}

/// Collects `PutByte` output into memory. `get_byte` returns EOF (255) like stdin EOF.
#[derive(Debug, Default)]
pub(crate) struct BufferOutputIo {
    /// Accumulated output bytes, in emission order.
    pub(crate) bytes: Vec<u8>,
}

impl RuntimeIo for BufferOutputIo {
    fn put_byte(&mut self, _ptr: isize, byte: u8) -> Result<(), IoError> {
        self.bytes.push(byte);
        Ok(())
    }

    fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
        Ok(EOF_BYTE)
    }
}
