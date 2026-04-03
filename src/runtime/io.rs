use std::io::{Read, Write};
use thiserror::Error;

const EOF_BYTE: u8 = 255;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("read error: {0}")]
    ReadError(String),
    #[error("write error: {0}")]
    WriteError(String),
}

/// Runtime io abs
pub trait RuntimeIo {
    fn put_byte(&mut self, byte: u8) -> Result<(), IoError>;
    fn get_byte(&mut self) -> Result<u8, IoError>;
}

pub struct StdIo;

impl StdIo {
    pub fn new() -> Self {
        Self
    }
}

impl RuntimeIo for StdIo {
    fn put_byte(&mut self, byte: u8) -> Result<(), IoError> {
        let mut out = std::io::stdout();
        out.write_all(&[byte])
            .map_err(|e| IoError::WriteError(e.to_string()))?;
        out.flush()
            .map_err(|e| IoError::WriteError(e.to_string()))?;
        Ok(())
    }

    fn get_byte(&mut self) -> Result<u8, IoError> {
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
pub struct BufferOutputIo {
    pub bytes: Vec<u8>,
}

impl RuntimeIo for BufferOutputIo {
    fn put_byte(&mut self, byte: u8) -> Result<(), IoError> {
        self.bytes.push(byte);
        Ok(())
    }

    fn get_byte(&mut self) -> Result<u8, IoError> {
        Ok(EOF_BYTE)
    }
}
