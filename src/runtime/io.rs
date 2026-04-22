//! Byte-oriented runtime I/O shared by the interpreter and compile-time folds.
//!
//! `RuntimeIo` is the abstract byte sink/source; `BufferedStdIo` is the
//! production stdio adaptor (4 KiB `BufWriter` / `BufReader` around the
//! process stdio — amortises `put_byte`/`get_byte` syscalls by ~3 orders
//! of magnitude on I/O-heavy BF programs), and `BufferOutputIo` buffers
//! output into memory (used by `-O3` stdout pre-folding and by tests).
//! The `ptr` argument lets implementations dispatch based on tape address,
//! enabling mmap-style side channels such as the GUI screen buffer.

use std::io::{BufRead, BufReader, BufWriter, Stdin, Stdout, Write, stdin, stdout};

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
    /// Flush any buffered writes to the underlying sink. Default: no-op for
    /// unbuffered or memory-backed implementations. Called once at program
    /// end so buffered adaptors (`BufferedStdIo`) can surface late-flush
    /// failures instead of swallowing them in `Drop`.
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

/// Capacity of the `BufferedStdIo` output buffer. Sized at 4 KiB so a
/// full buffer maps to one filesystem page / one pipe `write` on Linux.
const BUFFERED_STDIO_CAPACITY: usize = 4096;

/// Production stdio adaptor: 4 KiB `BufWriter` over stdout and a
/// `BufReader` over stdin. Large `.`-heavy programs drop from one
/// `write` syscall per byte to one per 4 KiB, cutting interpreter I/O
/// overhead by ~3 orders of magnitude on the matslina benchmark suite.
///
/// On `Drop` the inner `BufWriter` flushes best-effort and silently
/// discards errors — the interpreter's `run()` already calls
/// [`RuntimeIo::flush`] at program end so any write failure surfaces as
/// a `RuntimeError::Io` before drop.
pub(crate) struct BufferedStdIo {
    /// 4 KiB write buffer over the process stdout.
    writer: BufWriter<Stdout>,
    /// Line-buffered reader over the process stdin.
    reader: BufReader<Stdin>,
}

impl BufferedStdIo {
    /// Build a buffered stdio adaptor sharing the current process's
    /// stdin / stdout. The writer capacity is fixed at
    /// [`BUFFERED_STDIO_CAPACITY`]; the reader uses the stdlib default.
    pub(crate) fn new() -> Self {
        Self {
            writer: BufWriter::with_capacity(BUFFERED_STDIO_CAPACITY, stdout()),
            reader: BufReader::new(stdin()),
        }
    }
}

impl RuntimeIo for BufferedStdIo {
    fn put_byte(&mut self, _ptr: isize, byte: u8) -> Result<(), IoError> {
        self.writer
            .write_all(&[byte])
            .map_err(|e| IoError::WriteError(e.to_string()))
    }

    fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
        // `fill_buf` returns `&[]` at EOF, matching `StdIo` semantics
        // (255 on EOF). A partial read is impossible — `BufReader`
        // always delivers at least one byte when the upstream has any.
        let byte = match self.reader.fill_buf() {
            Ok([]) => return Ok(EOF_BYTE),
            Ok(buf) => buf[0],
            Err(e) => return Err(IoError::ReadError(e.to_string())),
        };
        self.reader.consume(1);
        Ok(byte)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.writer
            .flush()
            .map_err(|e| IoError::WriteError(e.to_string()))
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
