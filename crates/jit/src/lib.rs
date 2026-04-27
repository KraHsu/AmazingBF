//! JIT execution buffer for AmazingBF.
//!
//! Allocates executable memory via raw Linux syscalls (mmap/mprotect/munmap),
//! copies in pre-compiled x86_64 machine code, and jumps to it.
//!
//! This crate uses `#![deny(unsafe_code)]` with targeted `#[allow]` on the
//! functions that genuinely require unsafe: the raw syscall wrapper,
//! the memory copy, and the function-pointer transmute.

#![deny(unsafe_code)]

use std::fmt;

// Linux x86_64 syscall numbers
const SYS_MMAP: i64 = 9;
const SYS_MPROTECT: i64 = 10;
const SYS_MUNMAP: i64 = 11;

// mmap prot flags
const PROT_READ: i64 = 0x1;
const PROT_WRITE: i64 = 0x2;
const PROT_EXEC: i64 = 0x4;

// mmap flags
const MAP_PRIVATE: i64 = 0x02;
const MAP_ANONYMOUS: i64 = 0x20;

/// Errors that can occur during JIT buffer operations.
#[derive(Debug)]
pub enum JitError {
    /// The provided machine code is empty.
    EmptyCode,
    /// `mmap` syscall failed; carries the negated errno.
    MmapFailed(i64),
    /// `mprotect` syscall failed; carries the negated errno.
    MprotectFailed(i64),
    /// `munmap` syscall failed; carries the negated errno.
    MunmapFailed(i64),
}

impl fmt::Display for JitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCode => write!(f, "JIT: empty machine code"),
            Self::MmapFailed(e) => write!(f, "JIT: mmap failed (errno {})", -e),
            Self::MprotectFailed(e) => write!(f, "JIT: mprotect failed (errno {})", -e),
            Self::MunmapFailed(e) => write!(f, "JIT: munmap failed (errno {})", -e),
        }
    }
}

impl std::error::Error for JitError {}

/// Raw syscall wrapper for Linux x86_64.
///
/// # Safety
/// Caller must ensure the syscall number and arguments are valid.
#[allow(unsafe_code)]
unsafe fn raw_syscall6(nr: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

fn round_up_to_page(len: usize) -> usize {
    const PAGE: usize = 4096;
    (len + PAGE - 1) & !(PAGE - 1)
}

/// An executable memory buffer holding pre-compiled x86_64 machine code.
///
/// Created via [`JitBuffer::new`], which allocates RW pages, copies the code
/// in, then flips them to RX (W^X). Call [`JitBuffer::execute`] to jump into
/// the code. The buffer is `munmap`'d on drop.
pub struct JitBuffer {
    ptr: *mut u8,
    len: usize,
}

impl fmt::Debug for JitBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JitBuffer")
            .field("ptr", &self.ptr)
            .field("len", &self.len)
            .finish()
    }
}

impl JitBuffer {
    /// Allocate a JIT buffer, copy `text` into it, and make it executable.
    #[allow(unsafe_code)]
    pub fn new(text: &[u8]) -> Result<Self, JitError> {
        if text.is_empty() {
            return Err(JitError::EmptyCode);
        }

        let len = round_up_to_page(text.len());

        // mmap(NULL, len, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)
        let ptr = unsafe {
            raw_syscall6(
                SYS_MMAP,
                0,
                len as i64,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr < 0 {
            return Err(JitError::MmapFailed(ptr));
        }
        let ptr = ptr as *mut u8;

        // Copy machine code into the buffer
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), ptr, text.len());
        }

        // mprotect(ptr, len, PROT_READ|PROT_EXEC) — W^X flip
        let ret = unsafe {
            raw_syscall6(
                SYS_MPROTECT,
                ptr as i64,
                len as i64,
                PROT_READ | PROT_EXEC,
                0,
                0,
                0,
            )
        };
        if ret < 0 {
            // Clean up the mapping before returning the error
            unsafe {
                raw_syscall6(SYS_MUNMAP, ptr as i64, len as i64, 0, 0, 0, 0);
            }
            return Err(JitError::MprotectFailed(ret));
        }

        Ok(Self { ptr, len })
    }

    /// Jump into the compiled machine code.
    ///
    /// The generated code is expected to handle its own I/O and exit via
    /// `syscall(SYS_exit, 0)`. In that case this function never returns.
    /// If the code *does* return (e.g. via `ret`), this function returns `Ok(())`.
    #[allow(unsafe_code)]
    pub fn execute(&self) -> Result<(), JitError> {
        let f: extern "C" fn() = unsafe { core::mem::transmute(self.ptr) };
        f();
        Ok(())
    }
}

impl Drop for JitBuffer {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            raw_syscall6(SYS_MUNMAP, self.ptr as i64, self.len as i64, 0, 0, 0, 0);
        }
    }
}

// JitBuffer's raw pointer is only used for the mmap'd region which is
// process-private and not shared across threads.
#[allow(unsafe_code)]
unsafe impl Send for JitBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_code_is_rejected() {
        let err = JitBuffer::new(&[]).unwrap_err();
        assert!(matches!(err, JitError::EmptyCode));
    }

    #[test]
    fn round_up_page_sizes() {
        assert_eq!(round_up_to_page(1), 4096);
        assert_eq!(round_up_to_page(4096), 4096);
        assert_eq!(round_up_to_page(4097), 8192);
    }

    #[test]
    #[allow(unsafe_code)]
    fn execute_exit_zero() {
        // x86_64 machine code: mov rax, 60; xor rdi, rdi; syscall
        // This calls exit(0) — the process will exit, so we fork.
        let code: &[u8] = &[
            0x48, 0xC7, 0xC0, 0x3C, 0x00, 0x00, 0x00, // mov rax, 60
            0x48, 0x31, 0xFF, // xor rdi, rdi
            0x0F, 0x05, // syscall
        ];

        let buf = JitBuffer::new(code).expect("mmap should succeed");

        // Fork so the exit(0) doesn't kill the test runner
        let pid = unsafe { libc_fork() };
        if pid == 0 {
            // Child: execute the JIT code (will exit(0))
            let _ = buf.execute();
            std::process::exit(99);
        }
        // Parent: wait for child
        let status = unsafe { libc_waitpid(pid) };
        assert_eq!(status, 0, "child should exit with code 0");
    }

    /// Raw fork() via syscall — avoids libc dependency.
    #[allow(unsafe_code)]
    unsafe fn libc_fork() -> i32 {
        unsafe { raw_syscall6(57, 0, 0, 0, 0, 0, 0) as i32 }
    }

    /// Raw waitpid(pid, &status, 0) via syscall — returns exit code.
    #[allow(unsafe_code)]
    unsafe fn libc_waitpid(pid: i32) -> i32 {
        let mut status: i32 = 0;
        let status_ptr = &mut status as *mut i32;
        unsafe {
            raw_syscall6(61, pid as i64, status_ptr as i64, 0, 0, 0, 0);
        }
        // Extract exit code from wait status (bits 15:8)
        (status >> 8) & 0xFF
    }
}
