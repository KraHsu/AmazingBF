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
use crate::ir::lir::{LabelId, LirInst, LirProgram};

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

/// Lowest reserved internal-label ID (inclusive).
///
/// The range `[INTERNAL_LABEL_RESERVED_MIN_RAW, u32::MAX]` is reserved for
/// labels with fixed semantics and must never be consumed by
/// `fresh_internal_label()`.
const INTERNAL_LABEL_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_GROW_LOOP_RAW;

/// Starting point for transient internal label IDs.
///
/// The bounds check in `PtrAdd` needs temporary `slow_path` / `done` labels;
/// they are allocated downward from this base so they sit strictly below the
/// reserved range and cannot collide with fixed internal labels.
const INTERNAL_LABEL_BASE_RAW: u32 = INTERNAL_LABEL_RESERVED_MIN_RAW - 1;

/// Emit `PtrAdd`: move `r13`, calling `ensure_tape` on out-of-range.
fn emit_ptr_add_out(
    out: &mut Vec<AsmInst>,
    next_internal_label: &mut u32,
    n: isize,
    ensure_tape_label: AsmLabel,
) {
    if n == 0 {
        return;
    }

    let slow_path = fresh_internal_label(next_internal_label);
    let done = fresh_internal_label(next_internal_label);

    out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13));
    emit_add_reg_isize(out, Reg64::R15, n);

    out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R12));
    out.push(AsmInst::Jb(slow_path));

    out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R14));
    out.push(AsmInst::Jae(slow_path));

    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::R15));
    out.push(AsmInst::Jmp(done));

    out.push(AsmInst::Label(slow_path));
    out.push(AsmInst::Call(ensure_tape_label));

    out.push(AsmInst::Label(done));
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

    // Transient internal-label counter, decrementing from INTERNAL_LABEL_BASE_RAW.
    let mut next_internal_label = INTERNAL_LABEL_BASE_RAW;

    let mut out = Vec::new();

    // 1. Initialise the tape (mmap).
    emit_init_tape(&mut out, exit_one_label);

    // 2. Translate LIR instructions.
    for inst in &lir.insts {
        match inst {
            // PtrAdd(0): no-op.
            LirInst::PtrAdd(0) => {}

            // PtrAdd(n): move the data pointer.
            LirInst::PtrAdd(n) => {
                emit_ptr_add_out(&mut out, &mut next_internal_label, *n, ensure_tape_label);
            }

            // LinearMul: `-O1` affine loops (e.g. `[->+<]`).
            LirInst::LinearMul(factors) => {
                if factors.is_empty() {
                    out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                    continue;
                }
                out.push(AsmInst::Push(Reg64::Rbx));
                out.push(AsmInst::MovzxEbxFromMemR13);
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                for (off, f) in factors {
                    emit_ptr_add_out(&mut out, &mut next_internal_label, *off, ensure_tape_label);
                    out.push(AsmInst::MovEaxEbx);
                    out.push(AsmInst::ImulEaxEbxImm32(*f));
                    out.push(AsmInst::AddMemR13Al);
                    emit_ptr_add_out(&mut out, &mut next_internal_label, -*off, ensure_tape_label);
                }
                out.push(AsmInst::Pop(Reg64::Rbx));
            }

            // Scan: `while *p { < or > }` (`[<]` / `[>]`).
            LirInst::Scan(dir) => {
                let step = *dir;
                debug_assert!(step == 1 || step == -1, "Scan step must be ±1");
                let loop_top = fresh_internal_label(&mut next_internal_label);
                let loop_done = fresh_internal_label(&mut next_internal_label);
                out.push(AsmInst::Label(loop_top));
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(loop_done));
                emit_ptr_add_out(&mut out, &mut next_internal_label, step, ensure_tape_label);
                out.push(AsmInst::Jmp(loop_top));
                out.push(AsmInst::Label(loop_done));
            }

            // CellAdd(0): no-op.
            LirInst::CellAdd(0) => {}

            // CellAdd(n): modify the current cell.
            //
            // After merging, BF's `+` / `-` runs become a single `CellAdd(n)`
            // where `n` may be negative. Cells are 8-bit unsigned (0..=255),
            // so we only need `n mod 256`.
            LirInst::CellAdd(n) => {
                // Normalise into 0..=255; the extra `+ 256` handles negatives
                // (e.g. -1 → 255, -3 → 253).
                let imm = ((*n % 256) + 256) % 256;
                if imm != 0 {
                    // Equivalent to `*data_ptr = (*data_ptr + imm) % 256`.
                    out.push(AsmInst::AddMem8Imm8(Reg64::R13, imm as u8 as i8));
                }
            }

            // CellSet(v): overwrite the current cell.
            //
            // Emitted by patterns like `[-]` / `[+]`; writes the byte
            // directly, skipping the read-modify-write.
            LirInst::CellSet(v) => {
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, *v));
            }

            // PutByte: emit the current cell (`.`).
            //
            // Linux `sys_write(fd=1, buf=data_ptr, count=1)`.
            LirInst::PutByte => {
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 1)); // syscall number = 1 (write)
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // fd = 1 (stdout)
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13)); // buf = data_ptr
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1)); // count = 1
                out.push(AsmInst::Syscall);
            }

            // GetByte: read a byte into the current cell (`,`).
            //
            // Linux `sys_read(fd=0, buf=data_ptr, count=1)`. Semantics match
            // the interpreter:
            // - return 1: byte read, kernel already stored it at `*data_ptr`.
            // - return 0: EOF → store 255 in the current cell.
            // - return < 0: read failure → exit(1).
            LirInst::GetByte => {
                let done = fresh_internal_label(&mut next_internal_label);
                out.push(AsmInst::MovRegImm64(Reg64::Rax, 0)); // syscall number = 0 (read)
                out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // fd = 0 (stdin)
                out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R13)); // buf = data_ptr
                out.push(AsmInst::MovRegImm64(Reg64::Rdx, 1)); // count = 1
                out.push(AsmInst::Syscall);
                out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
                out.push(AsmInst::Jl(exit_one_label));
                out.push(AsmInst::Jnz(done));
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 255));
                out.push(AsmInst::Label(done));
            }

            // Label: label definition.
            //
            // Directly maps the LIR `LabelId` onto an `AsmLabel`.
            LirInst::Label(id) => {
                out.push(AsmInst::Label(map_label(*id)));
            }

            // JumpIfZero: jump if current cell is zero (BF `[`).
            LirInst::JumpIfZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0)); // compare *data_ptr with 0
                out.push(AsmInst::Jz(map_label(*id))); // jump if zero
            }

            // JumpIfNonZero: jump if current cell is non-zero (BF `]`).
            LirInst::JumpIfNonZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0)); // compare *data_ptr with 0
                out.push(AsmInst::Jnz(map_label(*id))); // jump if non-zero
            }
        }
    }

    // 3. Normal termination: exit(0).
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // syscall number = 60 (exit)
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 0)); // exit code = 0
    out.push(AsmInst::Syscall);

    // 4. Helper routines.
    emit_ensure_tape_contains_r15(&mut out, ensure_tape_label, exit_one_label);
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

/// Emit the OOM / failure path: `exit(1)`.
fn emit_exit_one(out: &mut Vec<AsmInst>, label: AsmLabel) {
    out.push(AsmInst::Label(label));
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 60)); // sys_exit = 60
    out.push(AsmInst::MovRegImm64(Reg64::Rdi, 1)); // exit code = 1
    out.push(AsmInst::Syscall);
}

/// Allocate a fresh internal label ID.
///
/// Takes the current value of `next_raw` and decrements it. Transient
/// internal labels must stay strictly below the reserved range, otherwise
/// they would collide with labels that carry fixed semantics (e.g.
/// `__grow_loop`).
fn fresh_internal_label(next_raw: &mut u32) -> AsmLabel {
    debug_assert!(
        *next_raw < INTERNAL_LABEL_RESERVED_MIN_RAW,
        "temporary internal label collided with reserved internal labels: raw=0x{next:08x}",
        next = *next_raw,
    );

    let label = AsmLabel(*next_raw);
    *next_raw -= 1;
    label
}

/// Map a LIR `LabelId` onto an `AsmLabel`.
///
/// The raw `u32` is reused as-is: user labels count up from 0 and internal
/// labels count down from `u32::MAX`, so the two namespaces never collide.
fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}

fn emit_add_reg_isize(out: &mut Vec<AsmInst>, reg: Reg64, value: isize) {
    let mut remaining = i64::try_from(value).expect("pointer delta did not fit in i64");

    while remaining != 0 {
        let chunk = if remaining > i64::from(i32::MAX) {
            i32::MAX
        } else if remaining < i64::from(i32::MIN) {
            i32::MIN
        } else {
            remaining as i32
        };

        out.push(AsmInst::AddRegImm32(reg, chunk));
        remaining -= i64::from(chunk);
    }
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
}
