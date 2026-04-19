use std::io::{Read, Write};

const EOF_BYTE: u8 = 255;

#[derive(Debug)]
pub(crate) enum IoError {
    ReadError(String),
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
    fn put_byte(&mut self, ptr: isize, byte: u8) -> Result<(), IoError>;
    fn get_byte(&mut self, ptr: isize) -> Result<u8, IoError>;
}

pub(crate) struct StdIo;

impl StdIo {
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
