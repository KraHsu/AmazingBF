//! LIR → x86_64 assembly code generator (codegen.rs).
//!
//! Translates the low-level IR (`LirProgram`) into an x86_64 assembly program
//! (`AsmProgram`).
//!
//! ## Compilation strategy
//!
//! The Brainfuck virtual machine is driven by two pieces of state: a
//! conceptually unbounded byte tape and a pointer into it. This backend
//! allocates the tape with `mmap` and grows it on demand whenever the pointer
//! would escape the currently mapped range.
//!
//! ### Register allocation
//!
//! A fixed register assignment is used (no real register allocator):
//!
//! | Register | Role              | Notes                                         |
//! |----------|-------------------|-----------------------------------------------|
//! | R12      | tape_base         | Buffer start returned by `mmap`               |
//! | R13      | data_ptr          | Current BF pointer (`>` / `<`)                |
//! | R14      | tape_end          | Buffer end = base + length                    |
//! | R15      | scratch           | Candidate `PtrAdd` target before bounds check |
//!
//! ### Tape growth strategy
//!
//! When a `PtrAdd` would move the pointer out of the current tape (either
//! below `base` or at/past `end`), `ensure_tape_contains_r15` is called:
//! 1. Repeatedly double the tape length until the target offset fits,
//! 2. `mmap` a fresh buffer of the new length,
//! 3. Copy the old contents into the middle of the new buffer,
//! 4. `munmap` the old buffer,
//! 5. Refresh R12 / R13 / R14 to point into the new buffer.
//!
//! ## Label allocation
//!
//! - User labels: mapped directly from LIR `LabelId` values (low `u32`s).
//! - Internal labels: numbered downwards from `u32::MAX`, so they never
//!   overlap user labels.

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::backend::codegen_common::{LabelAllocator, PlatformEmitter, emit_lir_body};
use crate::ir::lir::LirProgram;

/// Initial tape size in bytes.
///
/// 4096 bytes = one page, enough for most simple BF programs. Larger programs
/// hit `ensure_tape` and grow the buffer on demand.
const INITIAL_TAPE_SIZE: usize = 4096;

/// Internal label ID: entry point of the `ensure_tape` routine.
///
/// Called by the slow path of bounds-checked `PtrAdd`.
const INTERNAL_LABEL_ENSURE_TAPE_RAW: u32 = u32::MAX;

/// Internal label ID: the single failure exit (`exit(1)`).
///
/// Any `mmap` / `read` / `munmap` syscall that returns a negative value jumps
/// here.
const INTERNAL_LABEL_EXIT_ONE_RAW: u32 = u32::MAX - 1;

/// Internal label ID: the top of the doubling loop inside `ensure_tape`.
const INTERNAL_LABEL_GROW_LOOP_RAW: u32 = u32::MAX - 2;

/// Internal label ID: entry point of the `flush_output` helper.
///
/// Called by each `PutByte` when the 4 KiB output buffer fills, by every
/// `GetByte` (so prior `.` bytes reach stdout before `,` blocks on stdin),
/// and once on the normal `exit(0)` path. The helper emits a single
/// `write(1, buffer_base, rbx - buffer_base)` syscall and resets `rbx`.
const INTERNAL_LABEL_FLUSH_OUTPUT_RAW: u32 = u32::MAX - 3;

/// Lowest reserved internal-label ID (inclusive).
///
/// The range `[INTERNAL_LABEL_RESERVED_MIN_RAW, u32::MAX]` is reserved for
/// labels with fixed semantics and must never be consumed by
/// `fresh_internal_label()`.
const INTERNAL_LABEL_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_FLUSH_OUTPUT_RAW;

/// 4 KiB output buffer sized to match one filesystem page / the Linux
/// pipe atomic-write threshold.  The generated binary allocates one
/// such buffer via `mmap` at program start; `PutByte` accumulates into
/// it and flushes with a single `write` syscall on buffer-full, on
/// `GetByte` (to keep interactive prompts visible before stdin blocks),
/// and once on `exit(0)`.
const OUTPUT_BUFFER_SIZE: usize = 4096;

/// Starting point for transient internal label IDs.
///
/// The bounds check in `PtrAdd` needs temporary `slow_path` / `done` labels;
/// they are allocated downward from this base so they sit strictly below the
/// reserved range and cannot collide with fixed internal labels.
const INTERNAL_LABEL_BASE_RAW: u32 = INTERNAL_LABEL_RESERVED_MIN_RAW - 1;

struct LinuxEmitter;

impl PlatformEmitter for LinuxEmitter {
    fn emit_put_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        flush_output_label: AsmLabel,
    ) {
        let skip = labels.fresh();
        out.push(AsmInst::MovAlMemR13);
        out.push(AsmInst::MovMemRbxAl);
        out.push(AsmInst::AddRegImm32(Reg64::Rbx, 1));
        out.push(AsmInst::CmpRegReg(Reg64::Rbx, Reg64::Rbp));
        out.push(AsmInst::Jnz(skip));
        out.push(AsmInst::Call(flush_output_label));
        out.push(AsmInst::Label(skip));
    }

    fn emit_get_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        exit_one_label: AsmLabel,
        flush_output_label: AsmLabel,
    ) {
        out.push(AsmInst::Call(flush_output_label));
        let done = labels.fresh();
        out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
        out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0));
        out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13));
        out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1));
        out.push(AsmInst::Syscall);
        out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
        out.push(AsmInst::Jl(exit_one_label));
        out.push(AsmInst::Jnz(done));
        out.push(AsmInst::MovMem8Imm8(Reg64::R13, 255));
        out.push(AsmInst::Label(done));
    }

    fn needs_rsp_alignment(&self) -> bool {
        false
    }
}

/// Compile a LIR program into an x86_64 assembly program.
///
/// Produced layout:
/// ```text
/// [tape init]        ← emit_init_tape
/// [translated BF]    ← main loop
/// [exit(0)]          ← normal exit
/// [ensure_tape]      ← emit_ensure_tape_contains_r15
/// [exit(1)]          ← emit_exit_one (OOM / syscall failure)
/// ```
pub fn compile_lir_to_asm(lir: &LirProgram) -> AsmProgram {
    let ensure_tape_label = AsmLabel(INTERNAL_LABEL_ENSURE_TAPE_RAW);
    let exit_one_label = AsmLabel(INTERNAL_LABEL_EXIT_ONE_RAW);
    let flush_output_label = AsmLabel(INTERNAL_LABEL_FLUSH_OUTPUT_RAW);

    let mut labels = LabelAllocator::new(INTERNAL_LABEL_BASE_RAW, 0);

    let mut out = Vec::new();

    // 1. Initialise the tape (mmap) and the 4 KiB output buffer.
    emit_init_tape(&mut out, exit_one_label);
    emit_init_output_buffer(&mut out, exit_one_label);

    // 2. Translate LIR instructions (shared logic with Windows backend).
    let linux_emitter = LinuxEmitter;
    emit_lir_body(
        &mut out,
        &mut labels,
        lir,
        ensure_tape_label,
        flush_output_label,
        exit_one_label,
        &linux_emitter,
    );

    // 3. Normal termination: flush any buffered output, then exit(0).
    out.push(AsmInst::Call(flush_output_label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // syscall number = 60 (exit)
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // exit code = 0
    out.push(AsmInst::Syscall);

    // 4. Helper routines.
    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, exit_one_label);
    emit_flush_output(&mut out, flush_output_label);
    emit_exit_one(&mut out, exit_one_label);

    AsmProgram { insts: out }
}

/// Minimal ELF payload: just `exit(0)`. Used at `-O3` when the source has no
/// `.` operators at all.
pub fn compile_trivial_exit_asm() -> AsmProgram {
    AsmProgram {
        insts: vec![
            AsmInst::MovRegImm64(Reg64::Rax, 60),
            AsmInst::MovRegImm64(Reg64::Rdi, 0),
            AsmInst::Syscall,
        ],
    }
}

/// `write(1, buf, n); exit(0)`, with bytes living on the stack — the Brainfuck
/// tape is bypassed entirely.
///
/// An empty `data` degenerates to [`compile_trivial_exit_asm`].
pub fn compile_precomputed_stdout_asm(data: &[u8]) -> AsmProgram {
    if data.is_empty() {
        return compile_trivial_exit_asm();
    }
    AsmProgram {
        insts: vec![AsmInst::RawBytes(build_precomputed_stdout_machine_code(
            data,
        ))],
    }
}

/// Build raw `sub rsp` / `mov [rsp+off]` / `write` / `exit` bytes (Linux
/// x86_64).
fn build_precomputed_stdout_machine_code(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    let mut code = Vec::new();
    let alloc = (n + 15) & !15;

    emit_sub_rsp_imm32(&mut code, alloc as i32);

    for (i, &b) in data.iter().enumerate() {
        emit_mov_byte_rsp_offset(&mut code, i, b);
    }

    emit_mov_reg_imm64_low3(&mut code, 0, 1);
    emit_mov_reg_imm64_low3(&mut code, 7, 1);
    code.extend_from_slice(&[0x48, 0x89, 0xe6]);
    emit_mov_reg_imm64_low3(&mut code, 2, n as i64);

    code.extend_from_slice(&[0x0f, 0x05]);

    emit_add_rsp_imm32(&mut code, alloc as i32);

    emit_mov_reg_imm64_low3(&mut code, 0, 60);
    emit_mov_reg_imm64_low3(&mut code, 7, 0);
    code.extend_from_slice(&[0x0f, 0x05]);

    code
}

fn emit_mov_reg_imm64_low3(code: &mut Vec<u8>, reg_low3: u8, imm: i64) {
    debug_assert!(reg_low3 < 8);
    code.push(0x48);
    code.push(0xb8 + reg_low3);
    code.extend_from_slice(&imm.to_le_bytes());
}

fn emit_sub_rsp_imm32(code: &mut Vec<u8>, imm: i32) {
    debug_assert!(imm > 0);
    if imm <= 127 {
        code.extend_from_slice(&[0x48, 0x83, 0xec, imm as u8]);
    } else {
        code.push(0x48);
        code.push(0x81);
        code.push(0xec);
        code.extend_from_slice(&imm.to_le_bytes());
    }
}

fn emit_add_rsp_imm32(code: &mut Vec<u8>, imm: i32) {
    debug_assert!(imm > 0);
    if imm <= 127 {
        code.extend_from_slice(&[0x48, 0x83, 0xc4, imm as u8]);
    } else {
        code.push(0x48);
        code.push(0x81);
        code.push(0xc4);
        code.extend_from_slice(&imm.to_le_bytes());
    }
}

fn emit_mov_byte_rsp_offset(code: &mut Vec<u8>, offset: usize, val: u8) {
    if offset <= 127 {
        code.extend_from_slice(&[0xc6, 0x44, 0x24, offset as u8, val]);
    } else {
        code.push(0x48);
        code.extend_from_slice(&[0xc6, 0x84, 0x24]);
        code.extend_from_slice(&(offset as u32).to_le_bytes());
        code.push(val);
    }
}

/// Emit tape initialisation code.
///
/// Uses the `mmap` syscall to allocate the initial buffer:
/// ```c
/// void *ptr = mmap(NULL, INITIAL_TAPE_SIZE,
///                  PROT_READ | PROT_WRITE,
///                  MAP_PRIVATE | MAP_ANONYMOUS,
///                  -1, 0);
/// ```
///
/// On success the three core registers are initialised as:
/// - R12 = ptr            (tape base)
/// - R13 = ptr + size/2   (data pointer, starting in the middle of the tape)
/// - R14 = ptr + size     (tape end)
///
/// Starting the pointer in the middle rather than at the start means BF
/// programs that move the pointer left (`<`) can do so for a while before
/// triggering growth.
fn emit_init_tape(out: &mut Vec<AsmInst>, exit_one_label: AsmLabel) {
    // mmap syscall arguments.
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9)); // sys_mmap = 9
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // addr = NULL (let the kernel choose)
    out.push(AsmInst::MovRegImm64(
        // length = INITIAL_TAPE_SIZE
        Reg64::Rsi,
        INITIAL_TAPE_SIZE as i64,
    ));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3)); // prot = PROT_READ(1) | PROT_WRITE(2)
    out.push(AsmInst::MovRegImm64(
        // flags = MAP_PRIVATE(0x02) | MAP_ANONYMOUS(0x20)
        Reg64::R10,
        0x22,
    ));
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1)); // fd = -1 (anonymous mapping)
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0)); // offset = 0
    out.push(AsmInst::Syscall);

    // Check mmap return value; a negative return (e.g. -ENOMEM) is fatal.
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // Initialise registers.
    // R12 = tape_base = mmap return value.
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rax));

    // R13 = data_ptr = base + size/2 (middle of the tape).
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(
        Reg64::R13,
        (INITIAL_TAPE_SIZE / 2) as i32,
    ));

    // R14 = tape_end = base + size.
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::R14, INITIAL_TAPE_SIZE as i32));
}

/// Emit the tape-growth routine `ensure_tape_contains_r15`.
///
/// Precondition: R15 holds a target address that may sit outside the current
/// tape range.
///
/// ## Algorithm
///
/// 1. Compute `old_len = R14 - R12`.
/// 2. Compute `desired_offset = R15 - R12` (may be negative if R15 < R12).
/// 3. Repeatedly double `new_len` until:
///    - `copy_start = (new_len - old_len) / 2` (where the old contents will
///      land in the new tape),
///    - `copy_start + desired_offset` is in `[0, new_len)`.
/// 4. `mmap` a fresh buffer of length `new_len`.
/// 5. Copy the old bytes into the new buffer at offset `copy_start`.
/// 6. `munmap` the old buffer.
/// 7. Update `R12 = new_base`,
///    `R13 = new_base + copy_start + desired_offset`, `R14 = new_base + new_len`.
///
/// Postcondition: R12 / R13 / R14 reference the new tape and R13 is inside
/// the valid range.
fn emit_ensure_tape_contains_r15(
    out: &mut Vec<AsmInst>,
    ensure_tape_label: AsmLabel,
    exit_one_label: AsmLabel,
) {
    let grow_loop = AsmLabel(INTERNAL_LABEL_GROW_LOOP_RAW);

    // Function entry.
    out.push(AsmInst::Label(ensure_tape_label));

    // R10 = old_len = tape_end - tape_base.
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));

    // R9 = desired_offset = target_ptr - tape_base.
    // May be negative (when R15 < R12).
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));

    // R11 = new_len (candidate; doubles each iteration, seeded at old_len).
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::R10));

    // Doubling loop.
    out.push(AsmInst::Label(grow_loop));

    // new_len *= 2 (via self-add).
    out.push(AsmInst::AddRegReg(Reg64::R11, Reg64::R11));

    // R8 = copy_start = (new_len - old_len) / 2.
    // The old contents are centred inside the new buffer at this offset.
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // Check whether copy_start + desired_offset lies in [0, new_len).
    // RAX = copy_start + desired_offset.
    out.push(AsmInst::MovRegReg(Reg64::Rax, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::Rax, Reg64::R9));

    // rax < 0 (signed): new_len still too small.
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(grow_loop));

    // rax >= new_len (signed): new_len still too small.
    out.push(AsmInst::CmpRegReg(Reg64::Rax, Reg64::R11));
    out.push(AsmInst::Jge(grow_loop));

    // Allocate the new buffer.
    // mmap(NULL, new_len, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0).
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9)); // sys_mmap
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // addr = NULL
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R11)); // length = new_len
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3)); // prot
    out.push(AsmInst::MovRegImm64(Reg64::R10, 0x22)); // flags
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1)); // fd
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0)); // offset
    out.push(AsmInst::Syscall);

    // Check mmap return value.
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // Recompute values clobbered by the syscall.
    //
    // Linux x86_64 `syscall` clobbers `rcx` (holds old RIP) and `r11` (holds
    // old RFLAGS); mmap has also consumed r8 / r9 / r10. `rsi` still holds
    // `new_len` afterwards, so we recover:
    // - R10 = old_len (recomputed from R14 - R12)
    // - R9  = desired_offset (recomputed from R15 - R12)
    // - R11 = new_len (recovered from rsi)
    // - R8  = copy_start (recomputed as (new_len - old_len) / 2)
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::Rsi)); // rsi retained new_len
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));

    // RDX = new_base (from the mmap return).
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::Rax));

    // Copy the old contents into the new buffer.
    // rep movsb copies old_len bytes from old_base to new_base + copy_start.
    //
    // rep movsb inputs:
    // - RDI = destination = new_base + copy_start
    // - RSI = source      = old_base (R12)
    // - RCX = byte count  = old_len  (R10)
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::Rdx)); // rdi = new_base
    out.push(AsmInst::AddRegReg(Reg64::Rdi, Reg64::R8)); // rdi += copy_start
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R12)); // rsi = old_base
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::R10)); // rcx = old_len
    out.push(AsmInst::Cld); // clear DF (forward copy)
    out.push(AsmInst::RepMovsb); // do the copy

    // Compute the new tape_end before issuing munmap.
    // The upcoming syscall clobbers r11, so the value must be committed now.
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R14, Reg64::R11)); // r14 = new_base + new_len

    // Release the old buffer.
    // munmap(old_base, old_len).
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 11)); // sys_munmap = 11
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::R12)); // addr = old_base
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R10)); // length = old_len
    out.push(AsmInst::Syscall);
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // Refresh the remaining core registers.
    // R12 = new_base.
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rdx));

    // R13 = new_base + copy_start + desired_offset (target address inside new tape).
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R8)); // + copy_start
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R9)); // + desired_offset

    // Return to caller (the CALL in the PtrAdd slow path).
    out.push(AsmInst::Ret);
}

/// Emit the 4 KiB output-buffer allocation.
///
/// Runs once after [`emit_init_tape`]: mmaps a 4 KiB anonymous region and
/// pins the two buffer registers used by the `PutByte` fast path:
///
/// - `Rbx` = current write pointer, advanced by each `PutByte`; reset to
///   the buffer base by [`emit_flush_output`].
/// - `Rbp` = buffer end (= base + 4096); `PutByte`'s `cmp rbx, rbp`
///   detects a full buffer and triggers a flush.
///
/// On mmap failure the generated code jumps to `exit_one_label`, matching
/// the tape-allocation error path.
fn emit_init_output_buffer(out: &mut Vec<AsmInst>, exit_one_label: AsmLabel) {
    // mmap(NULL, OUTPUT_BUFFER_SIZE, PROT_READ|PROT_WRITE,
    //      MAP_PRIVATE|MAP_ANONYMOUS, -1, 0).
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 9)); // sys_mmap
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // addr = NULL
    out.push(AsmInst::MovRegImm64(Reg64::Rsi, OUTPUT_BUFFER_SIZE as i64));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0x3)); // prot = R|W
    out.push(AsmInst::MovRegImm64(Reg64::R10, 0x22)); // flags = PRIVATE|ANON
    out.push(AsmInst::MovRegImm64(Reg64::R8, -1)); // fd
    out.push(AsmInst::MovRegImm64(Reg64::R9, 0)); // offset
    out.push(AsmInst::Syscall);

    // mmap error: negative return → exit(1).
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(exit_one_label));

    // Rbx = buffer_base (write pointer starts at the beginning).
    out.push(AsmInst::MovRegReg(Reg64::Rbx, Reg64::Rax));

    // Rbp = buffer_base + OUTPUT_BUFFER_SIZE (buffer end sentinel).
    out.push(AsmInst::MovRegReg(Reg64::Rbp, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::Rbp, OUTPUT_BUFFER_SIZE as i32));
}

/// Emit the `flush_output` helper routine.
///
/// Called by `PutByte` on buffer-full, by every `GetByte` (so prior
/// output reaches stdout before stdin blocks), and once on `exit(0)`.
/// Writes `rbx - buffer_base` bytes to stdout and resets `rbx` to
/// `buffer_base`. When no bytes are pending (`rbx == buffer_base`),
/// `write(1, buf, 0)` is a harmless no-op.
///
/// The Linux syscall ABI preserves Rbx and Rbp across the syscall
/// (only Rcx/R11 are kernel scratch), so the helper can use
/// `lea rsi, [rbp - OUTPUT_BUFFER_SIZE]` to recover the base both
/// before the write and to reset the write pointer afterwards.
fn emit_flush_output(out: &mut Vec<AsmInst>, label: AsmLabel) {
    out.push(AsmInst::Label(label));

    // rsi = buffer_base = rbp - OUTPUT_BUFFER_SIZE.
    out.push(AsmInst::LeaRegMem(
        Reg64::Rsi,
        Reg64::Rbp,
        -(OUTPUT_BUFFER_SIZE as i32),
    ));

    // rdx = count = rbx - buffer_base.
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::Rbx));
    out.push(AsmInst::SubRegReg(Reg64::Rdx, Reg64::Rsi));

    // write(1, buffer_base, count).
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 1)); // sys_write
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // fd = stdout
    out.push(AsmInst::Syscall);

    // Reset write pointer to buffer_base.  (Ignoring short writes — BF
    // programs write to stdout, not to arbitrary file descriptors, so
    // EINTR / EAGAIN on a 4 KiB write is not a realistic concern.)
    out.push(AsmInst::MovRegReg(Reg64::Rbx, Reg64::Rsi));

    out.push(AsmInst::Ret);
}

/// Emit the OOM / failure path: `exit(1)`.
fn emit_exit_one(out: &mut Vec<AsmInst>, label: AsmLabel) {
    out.push(AsmInst::Label(label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // sys_exit = 60
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // exit code = 1
    out.push(AsmInst::Syscall);
}

// ---------------------------------------------------------------------------
// H2: ret-based JIT codegen
// ---------------------------------------------------------------------------

/// Internal label: JIT epilogue (restore callee-saved regs + ret).
const INTERNAL_LABEL_JIT_EPILOGUE_RAW: u32 = u32::MAX - 4;

/// Lowest reserved internal-label ID for JIT mode (inclusive).
const INTERNAL_LABEL_JIT_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_JIT_EPILOGUE_RAW;

/// Starting point for transient internal labels in JIT mode.
const INTERNAL_LABEL_JIT_BASE_RAW: u32 = INTERNAL_LABEL_JIT_RESERVED_MIN_RAW - 1;

/// Compile a LIR program into x86_64 assembly for JIT execution (H2).
///
/// The generated code is a SysV ABI function:
/// ```text
/// int32_t bf_main(uint8_t *tape_base, uint8_t *data_ptr, uint8_t *tape_end)
/// ```
///
/// Layout:
/// ```text
/// [prologue]         ← save callee-saved regs, load args into r12/r13/r14
/// [output buf init]  ← mmap 4 KiB output buffer
/// [translated BF]    ← main loop
/// [epilogue(0)]      ← flush, restore regs, return 0
/// [ensure_tape]      ← emit_ensure_tape_contains_r15
/// [flush_output]     ← emit_flush_output
/// [epilogue(1)]      ← restore regs, return 1 (OOM / syscall failure)
/// ```
pub fn compile_lir_to_jit_asm(lir: &LirProgram) -> AsmProgram {
    let ensure_tape_label = AsmLabel(INTERNAL_LABEL_ENSURE_TAPE_RAW);
    let exit_one_label = AsmLabel(INTERNAL_LABEL_EXIT_ONE_RAW);
    let flush_output_label = AsmLabel(INTERNAL_LABEL_FLUSH_OUTPUT_RAW);
    let epilogue_label = AsmLabel(INTERNAL_LABEL_JIT_EPILOGUE_RAW);

    let mut labels = LabelAllocator::new(INTERNAL_LABEL_JIT_BASE_RAW, 0);

    // 1. Function prologue: save callee-saved registers.
    //    SysV ABI callee-saved: rbx, rbp, r12-r15.
    // 2. Load tape state from SysV ABI arguments.
    //    rdi = tape_base, rsi = data_ptr, rdx = tape_end
    let mut out = vec![
        AsmInst::Push(Reg64::Rbp),
        AsmInst::Push(Reg64::Rbx),
        AsmInst::Push(Reg64::R12),
        AsmInst::Push(Reg64::R13),
        AsmInst::Push(Reg64::R14),
        AsmInst::Push(Reg64::R15),
        AsmInst::MovRegReg(Reg64::R12, Reg64::Rdi), // tape_base
        AsmInst::MovRegReg(Reg64::R13, Reg64::Rsi), // data_ptr
        AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx), // tape_end
    ];

    // 3. Allocate the 4 KiB output buffer (same as AOT path).
    emit_init_output_buffer(&mut out, exit_one_label);

    // 4. Translate LIR instructions.
    let linux_emitter = LinuxEmitter;
    emit_lir_body(
        &mut out,
        &mut labels,
        lir,
        ensure_tape_label,
        flush_output_label,
        exit_one_label,
        &linux_emitter,
    );

    // 5. Normal termination: flush output, return 0.
    out.push(AsmInst::Call(flush_output_label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
    out.push(AsmInst::Jmp(epilogue_label));

    // 6. Helper routines.
    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, exit_one_label);
    emit_flush_output(&mut out, flush_output_label);

    // 7. Error path: return 1 (replaces exit(1) syscall).
    out.push(AsmInst::Label(exit_one_label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 1));
    // Fall through to epilogue.

    // 8. Epilogue: restore callee-saved registers and return.
    out.push(AsmInst::Label(epilogue_label));
    out.push(AsmInst::Pop(Reg64::R15));
    out.push(AsmInst::Pop(Reg64::R14));
    out.push(AsmInst::Pop(Reg64::R13));
    out.push(AsmInst::Pop(Reg64::R12));
    out.push(AsmInst::Pop(Reg64::Rbx));
    out.push(AsmInst::Pop(Reg64::Rbp));
    out.push(AsmInst::Ret);

    AsmProgram { insts: out }
}

/// Compile a LIR program into a JIT loop body for the F1b tiered JIT.
///
/// Same SysV ABI as [`compile_lir_to_jit_asm`] (rdi=tape_base, rsi=data_ptr,
/// rdx=tape_end → i32) but tailored for hot-loop dispatch rather than
/// whole-program execution:
///
/// - **No output-buffer mmap**: hot loops are filtered by
///   [`crate::interp::jit_compile::analyse_eligibility`] which rejects any
///   `PutByte` / `GetByte`, so the 4 KiB write buffer the AOT/H2 path sets
///   up is never touched. Skipping the mmap avoids leaking 4 KiB on every
///   single JIT dispatch.
/// - **No flush call before return**: nothing is buffered, so flushing is
///   unnecessary.
/// - **No `flush_output` helper body**: dead code if the body never calls it.
///
/// `ensure_tape_contains_r15` is still emitted because `PtrAdd` codegen
/// references it on the slow path. The interpreter pre-grows the tape to
/// cover the loop's static reach before dispatching, so the slow path is
/// never taken in practice — but the label has to resolve.
pub fn compile_lir_to_jit_loop_asm(lir: &LirProgram) -> AsmProgram {
    let ensure_tape_label = AsmLabel(INTERNAL_LABEL_ENSURE_TAPE_RAW);
    let exit_one_label = AsmLabel(INTERNAL_LABEL_EXIT_ONE_RAW);
    let flush_output_label = AsmLabel(INTERNAL_LABEL_FLUSH_OUTPUT_RAW);
    let epilogue_label = AsmLabel(INTERNAL_LABEL_JIT_EPILOGUE_RAW);

    let mut labels = LabelAllocator::new(INTERNAL_LABEL_JIT_BASE_RAW, 0);

    let mut out = vec![
        AsmInst::Push(Reg64::Rbp),
        AsmInst::Push(Reg64::Rbx),
        AsmInst::Push(Reg64::R12),
        AsmInst::Push(Reg64::R13),
        AsmInst::Push(Reg64::R14),
        AsmInst::Push(Reg64::R15),
        AsmInst::MovRegReg(Reg64::R12, Reg64::Rdi),
        AsmInst::MovRegReg(Reg64::R13, Reg64::Rsi),
        AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx),
    ];

    let linux_emitter = LinuxEmitter;
    emit_lir_body(
        &mut out,
        &mut labels,
        lir,
        ensure_tape_label,
        flush_output_label,
        exit_one_label,
        &linux_emitter,
    );

    out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
    out.push(AsmInst::Jmp(epilogue_label));

    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, exit_one_label);

    out.push(AsmInst::Label(exit_one_label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 1));

    out.push(AsmInst::Label(epilogue_label));
    // SysV 16-byte struct return: status in rax, final data_ptr in rdx.
    // The host's `JitExit` reads rax into `status` and rdx into `data_ptr`.
    // Capture r13 BEFORE popping the callee-saved regs (after the pops r13
    // would already be the caller's value).
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::R13));
    out.push(AsmInst::Pop(Reg64::R15));
    out.push(AsmInst::Pop(Reg64::R14));
    out.push(AsmInst::Pop(Reg64::R13));
    out.push(AsmInst::Pop(Reg64::R12));
    out.push(AsmInst::Pop(Reg64::Rbx));
    out.push(AsmInst::Pop(Reg64::Rbp));
    out.push(AsmInst::Ret);

    AsmProgram { insts: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lir::{LirInst, LirProgram};

    #[test]
    fn get_byte_sets_eof_cell_to_255() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::GetByte],
        });

        let mut matched = false;
        for window in asm.insts.windows(5) {
            matched = matches!(
                window,
                [
                    AsmInst::CmpRegImm32(Reg64::Rax, 0),
                    AsmInst::Jl(_),
                    AsmInst::Jnz(_),
                    AsmInst::MovMem8Imm8(Reg64::R13, 255),
                    AsmInst::Label(_)
                ]
            );

            if matched {
                break;
            }
        }

        assert!(matched, "GetByte should map EOF to 255 in generated asm");
    }

    #[test]
    fn ptr_add_is_emitted_without_i32_truncation() {
        let large_delta = isize::try_from(i64::from(i32::MAX) + 5).unwrap();
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAdd(large_delta)],
        });

        let chunks: Vec<i32> = asm
            .insts
            .iter()
            .filter_map(|inst| match inst {
                AsmInst::AddRegImm32(Reg64::R15, imm) => Some(*imm),
                _ => None,
            })
            .collect();

        assert_eq!(chunks, vec![i32::MAX, 5]);
    }

    #[test]
    fn precomputed_stdout_asm_encodes_large_stack_offsets() {
        let data: Vec<u8> = (0..130).map(|i| (i % 256) as u8).collect();
        let asm = compile_precomputed_stdout_asm(&data);
        let encoded = crate::backend::x86_64::encode::encode_program(&asm);
        assert!(
            encoded.text.len() > 400,
            "130 output bytes should use disp8 + disp32 mov-to-[rsp+i] encodings"
        );
    }

    #[test]
    fn cell_add_at_emits_disp8_add() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellAddAt { off: 3, delta: 5 }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::AddMem8ImmDisp8(Reg64::R13, 3, 5)),
            "CellAddAt(3,5) should lower to AddMem8ImmDisp8(R13, 3, 5)"
        );
    }

    #[test]
    fn cell_set_at_emits_disp8_mov() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellSetAt { off: -4, val: 0x41 }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, -4, 0x41)),
            "CellSetAt(-4, 0x41) should lower to MovMem8ImmDisp8(R13, -4, 0x41)"
        );
    }

    #[test]
    fn cell_add_at_normalises_negative_delta() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellAddAt { off: 2, delta: -3 }],
        });
        // -3 mod 256 == 253 == 0xFD == -3 as i8.
        assert!(
            asm.insts
                .contains(&AsmInst::AddMem8ImmDisp8(Reg64::R13, 2, -3)),
            "CellAddAt(2,-3) should normalise delta and emit signed 0xFD byte"
        );
    }

    #[test]
    fn cell_add_at_with_zero_delta_emits_nothing() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellAddAt { off: 3, delta: 256 }],
        });
        for inst in &asm.insts {
            assert!(
                !matches!(inst, AsmInst::AddMem8ImmDisp8(_, 3, _)),
                "CellAddAt(3, 256) should produce no disp8 add (imm mod 256 == 0)"
            );
        }
    }

    /// Golden byte-string regression for `>>+<<<+` under the B4 + C3 pipeline.
    ///
    /// The mechanical lowering is
    /// `PtrAdd(2); CellAdd(1); PtrAdd(-3); CellAdd(1)`. After
    /// [`crate::ir::lir_postpone::postpone_pointer_adds`] the two writes land
    /// at offsets `-1` and `2` relative to the probed base, so the encoded
    /// `.text` must contain the two disp8 byte sequences
    /// `49 80 45 FF 01` and `49 80 45 02 01` (`add byte [r13 - 1], 1` and
    /// `add byte [r13 + 2], 1`). Catches regressions where either the pass
    /// forgets to emit displacement writes or the encoder drops the REX byte.
    #[test]
    fn postpone_plus_codegen_produces_disp8_add_bytes_for_scattered_writes() {
        use crate::ir::lir_opt::optimize_lir;
        use crate::ir::lir_postpone::postpone_pointer_adds;

        let input = LirProgram {
            insts: vec![
                LirInst::PtrAdd(2),
                LirInst::CellAdd(1),
                LirInst::PtrAdd(-3),
                LirInst::CellAdd(1),
            ],
        };
        let optimized = optimize_lir(postpone_pointer_adds(input));
        let asm = compile_lir_to_asm(&optimized);
        let encoded = crate::backend::x86_64::encode::encode_program(&asm);

        let add_at_neg1: &[u8] = &[0x49, 0x80, 0x45, 0xFF, 0x01];
        let add_at_pos2: &[u8] = &[0x49, 0x80, 0x45, 0x02, 0x01];
        assert!(
            encoded
                .text
                .windows(add_at_neg1.len())
                .any(|w| w == add_at_neg1),
            "expected disp8 add at [r13 - 1] (49 80 45 FF 01) in encoded .text"
        );
        assert!(
            encoded
                .text
                .windows(add_at_pos2.len())
                .any(|w| w == add_at_pos2),
            "expected disp8 add at [r13 + 2] (49 80 45 02 01) in encoded .text"
        );
    }

    #[test]
    fn ptr_add_checked_degenerate_window_emits_nothing() {
        // `lo_extent == 0 && hi_extent == 0` and `delta == 0` is a valid
        // degenerate form that codegen drops entirely.
        let baseline = compile_lir_to_asm(&LirProgram { insts: vec![] });
        let with_checked = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 0,
            }],
        });
        assert_eq!(baseline.insts, with_checked.insts);
    }

    #[test]
    fn ptr_add_checked_low_side_only_emits_single_base_compare() {
        // `lo_extent < 0, hi_extent == 0`: only the low probe fires; there is
        // exactly one `cmp r15, r12` (base) and no `cmp r15, r14` (end).
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -3,
                hi_extent: 0,
            }],
        });
        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 1);
        assert_eq!(end_cmps, 0);
    }

    #[test]
    fn ptr_add_checked_high_side_only_emits_single_end_compare() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 3,
            }],
        });
        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 0);
        assert_eq!(end_cmps, 1);
    }

    #[test]
    fn ptr_add_checked_both_sides_emits_one_of_each_compare() {
        // Both extents non-zero: exactly one compare per side — this is the
        // core M2 win over the prior 2–3 probe `PtrAdd` sequence (which would
        // have emitted two full `cmp-pair`s, i.e. four compares total).
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -2,
                hi_extent: 3,
            }],
        });
        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 1);
        assert_eq!(end_cmps, 1);
    }

    #[test]
    fn ptr_add_checked_advances_r13_unchecked() {
        // `delta != 0` must translate to a plain `add r13, delta` — not a
        // separate `MovRegReg R15, R13` + cmp + mov-back sequence. Checked
        // delta is the whole point of the op.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAddChecked {
                delta: 2,
                lo_extent: -2,
                hi_extent: 3,
            }],
        });
        let r13_adds: Vec<i32> = asm
            .insts
            .iter()
            .filter_map(|i| match i {
                AsmInst::AddRegImm32(Reg64::R13, imm) => Some(*imm),
                _ => None,
            })
            .collect();
        assert!(
            r13_adds.contains(&2),
            "expected `add r13, 2` for fast-path delta; got {r13_adds:?}"
        );
    }

    #[test]
    fn postpone_plus_codegen_emits_single_checked_probe_for_zigzag() {
        // `PtrAdd(3); +; PtrAdd(-5); +; PtrAdd(2)` zigzags over offsets in
        // [-2, 3] with net virt_ptr = 0. After B4 the pass emits exactly one
        // `PtrAddChecked`, which lowers to one `cmp r15, r12` + one
        // `cmp r15, r14`. Regression against re-introducing the 3-probe path.
        use crate::ir::lir_postpone::postpone_pointer_adds;

        let input = LirProgram {
            insts: vec![
                LirInst::PtrAdd(3),
                LirInst::CellAdd(1),
                LirInst::PtrAdd(-5),
                LirInst::CellAdd(1),
                LirInst::PtrAdd(2),
            ],
        };
        let lir = postpone_pointer_adds(input);
        let asm = compile_lir_to_asm(&lir);

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(
            base_cmps, 1,
            "zigzag run should emit exactly one base-side bounds compare"
        );
        assert_eq!(
            end_cmps, 1,
            "zigzag run should emit exactly one end-side bounds compare"
        );
    }

    #[test]
    fn ptr_add_inside_verified_window_skips_bounds_check() {
        // After `PtrAddChecked { lo=-2, hi=3 }` the codegen remembers that the
        // window `[-2, 3]` around `r13` is mapped. A follow-up `PtrAdd(2)` lies
        // inside that window, so it must lower to a bare `add r13, 2` — no
        // `cmp r15, r12` / `cmp r15, r14` pair, and no `mov r15, r13` probe.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -2,
                    hi_extent: 3,
                },
                LirInst::PtrAdd(2),
            ],
        });

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(
            base_cmps, 1,
            "only the PtrAddChecked probe should emit a base-side compare; got {base_cmps}"
        );
        assert_eq!(
            end_cmps, 1,
            "only the PtrAddChecked probe should emit an end-side compare; got {end_cmps}"
        );
    }

    #[test]
    fn ptr_add_outside_verified_window_still_probes() {
        // `PtrAddChecked { lo=0, hi=2 }` verifies `[0, 2]`. A subsequent
        // `PtrAdd(3)` lands outside that window, so it must emit its own probe.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: 0,
                    hi_extent: 2,
                },
                LirInst::PtrAdd(3),
            ],
        });

        // PtrAddChecked(0, 2) emits only the end-side probe (1 cmp r15, r14).
        // PtrAdd(3) emits both sides (1 cmp r15, r12 and 1 cmp r15, r14).
        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 1, "PtrAdd(3) falls outside window → base probe");
        assert_eq!(
            end_cmps, 2,
            "both PtrAddChecked end probe and PtrAdd(3) end probe should fire"
        );
    }

    #[test]
    fn verified_window_cleared_by_label_barrier() {
        // A `Label` is a control-flow barrier: after it the verified window is
        // unknown because execution may reach the label from elsewhere. The
        // following `PtrAdd(2)`, despite being inside the original window,
        // must re-emit its bounds check.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -2,
                    hi_extent: 3,
                },
                LirInst::Label(crate::ir::lir::LabelId(0)),
                LirInst::PtrAdd(2),
            ],
        });

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        // PtrAddChecked emits 1 base + 1 end; post-barrier PtrAdd emits 1 base
        // + 1 end of its own.
        assert_eq!(
            base_cmps, 2,
            "label barrier must force re-probe on following PtrAdd"
        );
        assert_eq!(
            end_cmps, 2,
            "label barrier must force re-probe on following PtrAdd"
        );
    }

    #[test]
    fn verified_window_cleared_by_putbyte_barrier() {
        // PutByte is an I/O barrier; after it the window must be invalidated
        // because the slow path's `ensure_tape` may have relocated `r13`
        // (though PutByte itself doesn't, later codegen contracts treat any
        // syscall site as a barrier — keep the contract tight).
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -2,
                    hi_extent: 3,
                },
                LirInst::PutByte,
                LirInst::PtrAdd(2),
            ],
        });

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 2);
        assert_eq!(end_cmps, 2);
    }

    #[test]
    fn second_ptr_add_checked_covered_by_first_emits_no_probes() {
        // First op verifies `[-3, 5]`; the second op's window `[-1, 2]` is a
        // strict subset → its probes should be elided entirely. The second op
        // still advances `r13` by `delta = 1` via a plain `add r13, 1`.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -3,
                    hi_extent: 5,
                },
                LirInst::PtrAddChecked {
                    delta: 1,
                    lo_extent: -1,
                    hi_extent: 2,
                },
            ],
        });

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(
            base_cmps, 1,
            "second PtrAddChecked's base probe should be elided"
        );
        assert_eq!(
            end_cmps, 1,
            "second PtrAddChecked's end probe should be elided"
        );
    }

    #[test]
    fn cell_writes_do_not_clear_verified_window() {
        // CellAdd / CellAddAt / CellSet / CellSetAt do not touch `r13`, so the
        // verified window must survive them. A following in-window `PtrAdd`
        // should still elide its bounds check.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -2,
                    hi_extent: 3,
                },
                LirInst::CellAdd(1),
                LirInst::CellAddAt { off: 2, delta: 3 },
                LirInst::CellSet(0),
                LirInst::PtrAdd(2),
            ],
        });

        let base_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R12)))
            .count();
        let end_cmps = asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CmpRegReg(Reg64::R15, Reg64::R14)))
            .count();
        assert_eq!(base_cmps, 1, "cell writes are transparent to the window");
        assert_eq!(end_cmps, 1, "cell writes are transparent to the window");
    }

    #[test]
    fn cell_add_at_wide_off_selects_disp32() {
        let off: isize = 1_000;
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellAddAt { off, delta: 5 }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::AddMem8ImmDisp32(Reg64::R13, 1_000, 5)),
            "off beyond i8 range must lower to AddMem8ImmDisp32"
        );
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::AddMem8ImmDisp8(_, _, _))),
            "no disp8 variant should appear when off does not fit i8"
        );
    }

    #[test]
    fn cell_set_at_wide_off_selects_disp32() {
        let off: isize = -10_000;
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellSetAt { off, val: 0x7F }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp32(Reg64::R13, -10_000, 0x7F)),
            "negative off beyond i8 range must lower to MovMem8ImmDisp32"
        );
    }

    #[test]
    fn cell_add_at_narrow_off_keeps_disp8() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::CellAddAt { off: 5, delta: 3 }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::AddMem8ImmDisp8(Reg64::R13, 5, 3)),
            "off that fits i8 must stay on the disp8 path"
        );
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::AddMem8ImmDisp32(_, _, _))),
            "no disp32 variant should appear for small offsets"
        );
    }

    #[test]
    fn ensure_tape_does_not_use_r11_after_munmap_syscall() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::PtrAdd(4096)],
        });

        let munmap_idx = asm
            .insts
            .iter()
            .position(|inst| matches!(inst, AsmInst::MovRegImm64(Reg64::Rax, 11)))
            .expect("expected munmap syscall sequence");

        let tail = &asm.insts[munmap_idx..];
        let mut saw_syscall = false;

        for inst in tail {
            if matches!(inst, AsmInst::Syscall) {
                saw_syscall = true;
                continue;
            }

            if saw_syscall {
                assert!(
                    !matches!(inst, AsmInst::AddRegReg(Reg64::R14, Reg64::R11)),
                    "r11 is clobbered by syscall and must not be used after munmap"
                );
            }
        }
    }

    #[test]
    fn scan_with_hint_positive_dir_sets_up_repne_scasb_forward() {
        // ScanWithHint(dir=+1, hint=5) under D2 must:
        //   1. zero al (the comparand) and load rdi=r13, rcx=5
        //   2. clear DF (forward direction) before the scan
        //   3. emit `repne scasb` (the SIMD loop)
        //   4. recover r13 = rdi - step (= rdi - 1 here) and funnel into
        //      the slow_top fallthrough.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 5,
            }],
        });

        let setup = asm.insts.windows(5).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegImm64(Reg64::Rax, 0),
                    AsmInst::MovRegReg(Reg64::Rdi, Reg64::R13),
                    AsmInst::MovRegImm64(Reg64::Rcx, 5),
                    AsmInst::Cld,
                    AsmInst::RepneScasb,
                ]
            )
        });
        assert!(
            setup,
            "ScanWithHint(+1, 5) must set up `al=0; rdi=r13; rcx=5; cld; repne scasb`"
        );

        let recovers = asm.insts.windows(2).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegReg(Reg64::R13, Reg64::Rdi),
                    AsmInst::AddRegImm32(Reg64::R13, -1),
                ]
            )
        });
        assert!(
            recovers,
            "after scasb, r13 must be set to rdi - 1 (back up the post-incremented rdi)"
        );

        // No leftover R10 boundary compare from the legacy fast path
        // (slow_top's bounds check still uses R15 / Jae against R12/R14;
        // those are not the boundary compare we're checking for here).
        assert!(
            !asm.insts.iter().any(|i| matches!(
                i,
                AsmInst::CmpRegReg(Reg64::R13, Reg64::R10)
                    | AsmInst::CmpRegReg(Reg64::R10, Reg64::R13)
            )),
            "ScanWithHint must no longer reference the legacy R10 boundary compare"
        );
    }

    #[test]
    fn scan_with_hint_negative_dir_uses_std_then_restores_cld() {
        // dir=-1 needs DF=1 during the scan and DF=0 restored before any
        // function-call boundary in the slow_top fallthrough. r13 is
        // recovered as rdi - step = rdi + 1.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: -1,
                hint_bytes: 4,
            }],
        });

        let std_then_scasb_then_cld = asm
            .insts
            .windows(3)
            .any(|w| matches!(w, [AsmInst::Std, AsmInst::RepneScasb, AsmInst::Cld]));
        assert!(
            std_then_scasb_then_cld,
            "ScanWithHint(-1) must bracket scasb with `std` ... `cld` to honour the SysV/Win64 ABI"
        );

        let recovers = asm.insts.windows(2).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegReg(Reg64::R13, Reg64::Rdi),
                    AsmInst::AddRegImm32(Reg64::R13, 1),
                ]
            )
        });
        assert!(recovers, "after backward scasb, r13 must be set to rdi + 1");
    }

    #[test]
    fn scan_with_hint_zero_hint_emits_only_slow_body() {
        // hint_bytes == 0: no SIMD setup; we only emit the slow body
        // (`cmp [r13], 0; jz done; ptr_add; jmp slow_top`).
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 0,
            }],
        });
        assert!(
            !asm.insts.iter().any(|i| matches!(i, AsmInst::RepneScasb)),
            "hint=0 must not emit the SIMD `repne scasb` setup"
        );
        assert!(
            !asm.insts.iter().any(|i| matches!(i, AsmInst::Std)),
            "hint=0 must not flip the direction flag"
        );
    }

    #[test]
    fn linear_mul_factor_one_emits_add_mem_r13_bl_disp8_skipping_imul() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(2, 1)])],
        });
        assert!(
            asm.insts.contains(&AsmInst::AddMemR13BlDisp8(2)),
            "LinearMul factor=1 must emit AddMemR13BlDisp8"
        );
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::ImulEaxEbxImm32(_))),
            "LinearMul factor=1 must not emit `imul` for the ±1 column"
        );
    }

    #[test]
    fn linear_mul_factor_minus_one_emits_sub_mem_r13_bl_disp8() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(3, -1)])],
        });
        assert!(
            asm.insts.contains(&AsmInst::SubMemR13BlDisp8(3)),
            "LinearMul factor=-1 must emit SubMemR13BlDisp8"
        );
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::ImulEaxEbxImm32(_))),
            "LinearMul factor=-1 must not emit `imul` for the ±1 column"
        );
    }

    #[test]
    fn linear_mul_other_factor_keeps_imul_path() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(1, 7)])],
        });
        assert!(
            asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::ImulEaxEbxImm32(7))),
            "non-±1 factor must keep the imul path"
        );
        assert!(
            !asm.insts.iter().any(|i| matches!(
                i,
                AsmInst::AddMemR13Bl
                    | AsmInst::SubMemR13Bl
                    | AsmInst::AddMemR13BlDisp8(_)
                    | AsmInst::SubMemR13BlDisp8(_)
            )),
            "non-±1 factor must not emit ±bl variants"
        );
    }

    #[test]
    fn linear_mul_mixed_columns_pick_per_column() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(1, 1), (2, -1), (3, 5)])],
        });
        assert!(asm.insts.contains(&AsmInst::AddMemR13BlDisp8(1)));
        assert!(asm.insts.contains(&AsmInst::SubMemR13BlDisp8(2)));
        assert!(asm.insts.contains(&AsmInst::ImulEaxEbxImm32(5)));
    }

    #[test]
    fn zero_run_count_at_least_16_uses_rep_stosb() {
        // count=16 hits the D2 SIMD threshold: emit
        // `xor eax, eax; lea rdi,[r13+start]; mov ecx, count; cld; rep stosb`.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ZeroRun {
                start: 0,
                count: 16,
            }],
        });
        let simd = asm.insts.windows(5).any(|w| {
            matches!(
                w,
                [
                    AsmInst::XorEaxEax,
                    AsmInst::LeaRegMem(Reg64::Rdi, Reg64::R13, 0),
                    AsmInst::MovEcxImm32(16),
                    AsmInst::Cld,
                    AsmInst::RepStosb,
                ]
            )
        });
        assert!(
            simd,
            "ZeroRun with count >= 16 must lower to the rep stosb SIMD form"
        );
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::MovMem8ImmDisp8(_, _, _))),
            "SIMD ZeroRun must not emit per-byte disp8 stores for the same range"
        );
    }

    #[test]
    fn zero_run_count_below_threshold_keeps_scalar_stores() {
        // count=8 stays below the 16-byte threshold, so the scalar peeled
        // form is preferred (no rep stosb / xor eax, eax).
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ZeroRun { start: 0, count: 8 }],
        });
        assert!(
            !asm.insts.iter().any(|i| matches!(i, AsmInst::RepStosb)),
            "ZeroRun(count=8) must stay on the scalar store path"
        );
    }

    #[test]
    fn zero_run_at_origin_emits_base_store_plus_disp8_tail() {
        // ZeroRun { start: 0, count: 3 } → bare [r13]=0, then disp8 stores at
        // r13+1 and r13+2.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ZeroRun { start: 0, count: 3 }],
        });
        assert!(
            asm.insts.contains(&AsmInst::MovMem8Imm8(Reg64::R13, 0)),
            "ZeroRun covering offset 0 must emit a bare [r13]=0 store"
        );
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, 1, 0)),
            "ZeroRun starting at 0 with count 3 must emit a disp8 store at +1"
        );
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, 2, 0)),
            "ZeroRun starting at 0 with count 3 must emit a disp8 store at +2"
        );
    }

    #[test]
    fn zero_run_negative_start_emits_only_disp_stores() {
        // start=-2, count=2 covers [-2, -1]; neither offset is 0 so both go
        // through the disp8 path and no bare [r13]=0 store appears.
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ZeroRun {
                start: -2,
                count: 2,
            }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, -2, 0)),
            "expected disp8 store at r13 - 2"
        );
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, -1, 0)),
            "expected disp8 store at r13 - 1"
        );
        // ZeroRun introduces no [r13]=0 when the origin isn't in the range.
        // (The init-tape prologue uses MovRegImm64 / AddRegImm32, not MovMem8Imm8.)
        assert!(
            !asm.insts
                .iter()
                .any(|i| matches!(i, AsmInst::MovMem8Imm8(Reg64::R13, 0))),
            "ZeroRun not covering offset 0 must not emit [r13]=0"
        );
    }

    #[test]
    fn zero_run_wide_span_selects_disp32() {
        let asm = compile_lir_to_asm(&LirProgram {
            insts: vec![LirInst::ZeroRun {
                start: 200,
                count: 2,
            }],
        });
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp32(Reg64::R13, 200, 0)),
            "offset beyond i8 range must lower to disp32 store"
        );
    }

    #[test]
    fn peephole_plus_codegen_collapses_adjacent_zero_writes_to_zero_run() {
        // End-to-end: the postpone pass emits `CellSet(0); CellSetAt(1, 0);
        // CellSetAt(2, 0)` for `[-]>[-]>[-]`-style source, and the peephole
        // must coalesce them into ZeroRun { start: 0, count: 3 }.
        use crate::ir::lir_opt::optimize_lir;
        let optimized = optimize_lir(LirProgram {
            insts: vec![
                LirInst::CellSet(0),
                LirInst::CellSetAt { off: 1, val: 0 },
                LirInst::CellSetAt { off: 2, val: 0 },
            ],
        });
        assert_eq!(
            optimized.insts,
            vec![LirInst::ZeroRun { start: 0, count: 3 }]
        );
        let asm = compile_lir_to_asm(&optimized);
        assert!(
            asm.insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, 2, 0)),
            "ZeroRun must still produce a store at +2 after codegen"
        );
    }

    #[test]
    fn ptr_add_checked_then_scan_lowers_to_scan_with_hint_via_pipeline() {
        // End-to-end: a PtrAddChecked that verifies [-2, 3] followed by Scan(+1)
        // must go through `promote_scan_hints` and reach codegen as
        // ScanWithHint(+1, 3), which D2 lowers to a `repne scasb` with rcx=3.
        use crate::ir::lir_scan_hint::promote_scan_hints;

        let lir = promote_scan_hints(LirProgram {
            insts: vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -2,
                    hi_extent: 3,
                },
                LirInst::Scan(1),
            ],
        });
        let asm = compile_lir_to_asm(&lir);

        let seeds_rcx_with_hint = asm.insts.windows(3).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegImm64(Reg64::Rcx, 3),
                    AsmInst::Cld,
                    AsmInst::RepneScasb,
                ]
            )
        });
        assert!(
            seeds_rcx_with_hint,
            "pipeline must promote Scan(+1) after PtrAddChecked([-2, 3]) to a `rcx=3; cld; repne scasb` SIMD scan"
        );
    }

    // --- H2 JIT codegen tests ---

    #[test]
    fn jit_asm_starts_with_callee_saved_pushes() {
        let asm = compile_lir_to_jit_asm(&LirProgram { insts: vec![] });
        assert_eq!(asm.insts[0], AsmInst::Push(Reg64::Rbp));
        assert_eq!(asm.insts[1], AsmInst::Push(Reg64::Rbx));
        assert_eq!(asm.insts[2], AsmInst::Push(Reg64::R12));
        assert_eq!(asm.insts[3], AsmInst::Push(Reg64::R13));
        assert_eq!(asm.insts[4], AsmInst::Push(Reg64::R14));
        assert_eq!(asm.insts[5], AsmInst::Push(Reg64::R15));
    }

    #[test]
    fn jit_asm_loads_tape_from_sysv_args() {
        let asm = compile_lir_to_jit_asm(&LirProgram { insts: vec![] });
        assert_eq!(asm.insts[6], AsmInst::MovRegReg(Reg64::R12, Reg64::Rdi));
        assert_eq!(asm.insts[7], AsmInst::MovRegReg(Reg64::R13, Reg64::Rsi));
        assert_eq!(asm.insts[8], AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx));
    }

    #[test]
    fn jit_asm_ends_with_epilogue_and_ret() {
        let asm = compile_lir_to_jit_asm(&LirProgram { insts: vec![] });
        let n = asm.insts.len();
        assert_eq!(asm.insts[n - 1], AsmInst::Ret);
        assert_eq!(asm.insts[n - 2], AsmInst::Pop(Reg64::Rbp));
        assert_eq!(asm.insts[n - 3], AsmInst::Pop(Reg64::Rbx));
        assert_eq!(asm.insts[n - 4], AsmInst::Pop(Reg64::R12));
        assert_eq!(asm.insts[n - 5], AsmInst::Pop(Reg64::R13));
        assert_eq!(asm.insts[n - 6], AsmInst::Pop(Reg64::R14));
        assert_eq!(asm.insts[n - 7], AsmInst::Pop(Reg64::R15));
    }

    #[test]
    fn jit_asm_has_no_exit_syscall() {
        let asm = compile_lir_to_jit_asm(&LirProgram {
            insts: vec![LirInst::CellAdd(1)],
        });
        let has_exit_syscall = asm.insts.windows(3).any(|w| {
            matches!(
                w,
                [AsmInst::MovRegImm64(Reg64::Rax, 60), _, AsmInst::Syscall]
            )
        });
        assert!(
            !has_exit_syscall,
            "JIT codegen must not emit exit(0) or exit(1) syscalls"
        );
    }

    #[test]
    fn jit_asm_normal_exit_returns_zero() {
        let asm = compile_lir_to_jit_asm(&LirProgram { insts: vec![] });
        let has_mov_rax_0_then_jmp = asm
            .insts
            .windows(2)
            .any(|w| matches!(w, [AsmInst::MovRegImm64(Reg64::Rax, 0), AsmInst::Jmp(_)]));
        assert!(
            has_mov_rax_0_then_jmp,
            "JIT normal exit must set rax=0 then jump to epilogue"
        );
    }
}
