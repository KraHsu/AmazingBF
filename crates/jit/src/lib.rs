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

    /// Jump into the compiled machine code (H1 legacy entry point).
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

    /// Call the JIT code as a SysV ABI function that receives tape state
    /// and returns an exit code (H2 ret-based entry point).
    ///
    /// The generated code must follow the convention:
    /// - `rdi` = tape_base, `rsi` = data_ptr, `rdx` = tape_end
    /// - Returns 0 on success, 1 on error (OOM / syscall failure).
    ///
    /// The caller retains ownership of the tape memory; the JIT code may
    /// reallocate it via mmap/munmap (the tape-growth routine), so the
    /// returned pointers (via the out-params baked into the generated code)
    /// may differ from the inputs.
    #[allow(unsafe_code)]
    pub fn execute_fn(
        &self,
        tape_base: *mut u8,
        data_ptr: *mut u8,
        tape_end: *mut u8,
    ) -> i32 {
        let f: extern "C" fn(*mut u8, *mut u8, *mut u8) -> i32 =
            unsafe { core::mem::transmute(self.ptr) };
        f(tape_base, data_ptr, tape_end)
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

/// Allocate a zero-initialised anonymous RW mapping of `size` bytes.
///
/// Returns a non-null pointer on success, or null on failure.
/// The caller is responsible for eventually `munmap`-ing the region.
///
/// # Safety
/// `size` must be > 0. The returned pointer is valid for `size` bytes.
#[allow(unsafe_code)]
pub unsafe fn mmap_anonymous_rw(size: usize) -> *mut u8 {
    let ret = unsafe {
        raw_syscall6(
            SYS_MMAP,
            0,
            size as i64,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ret < 0 {
        core::ptr::null_mut()
    } else {
        ret as *mut u8
    }
}

/// A zero-initialised RW memory region for use as a BF tape in JIT mode.
///
/// The data pointer starts in the middle of the tape so that both `<` and `>`
/// have room before triggering growth. The JIT-generated code may reallocate
/// the tape via its own mmap/munmap (the `ensure_tape` routine), so the
/// original pointers may become stale after execution.
pub struct JitTape {
    ptr: *mut u8,
    len: usize,
}

impl JitTape {
    /// Allocate a new tape of `size` bytes via mmap.
    #[allow(unsafe_code)]
    pub fn new(size: usize) -> Result<Self, JitError> {
        let ptr = unsafe { mmap_anonymous_rw(size) };
        if ptr.is_null() {
            return Err(JitError::MmapFailed(-1));
        }
        Ok(Self { ptr, len: size })
    }

    /// Base address of the tape.
    pub fn base(&self) -> *mut u8 {
        self.ptr
    }

    /// Initial data pointer (middle of the tape).
    #[allow(unsafe_code)]
    pub fn data_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.add(self.len / 2) }
    }

    /// End address of the tape (one past the last byte).
    #[allow(unsafe_code)]
    pub fn end(&self) -> *mut u8 {
        unsafe { self.ptr.add(self.len) }
    }
}

impl Drop for JitTape {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            raw_syscall6(SYS_MUNMAP, self.ptr as i64, self.len as i64, 0, 0, 0, 0);
        }
    }
}

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

    #[test]
    fn execute_fn_returns_value() {
        // x86_64: mov eax, 42; ret
        // A minimal function that returns 42 via the SysV ABI.
        let code: &[u8] = &[
            0xB8, 0x2A, 0x00, 0x00, 0x00, // mov eax, 42
            0xC3, // ret
        ];
        let buf = JitBuffer::new(code).expect("mmap should succeed");
        let ret = buf.execute_fn(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert_eq!(ret, 42, "execute_fn should return the value from eax");
    }

    #[test]
    fn execute_fn_receives_args() {
        // x86_64: mov rax, rdi; add rax, rsi; add rax, rdx; ret
        // Returns the sum of the three pointer arguments (as integers).
        let code: &[u8] = &[
            0x48, 0x89, 0xF8, // mov rax, rdi
            0x48, 0x01, 0xF0, // add rax, rsi
            0x48, 0x01, 0xD0, // add rax, rdx
            0xC3, // ret
        ];
        let buf = JitBuffer::new(code).expect("mmap should succeed");
        let ret = buf.execute_fn(1 as *mut u8, 2 as *mut u8, 3 as *mut u8);
        assert_eq!(ret, 6, "execute_fn should pass args via rdi/rsi/rdx");
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
