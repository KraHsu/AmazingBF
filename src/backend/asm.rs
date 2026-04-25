//! Assembly IR for the x86_64 backend.
//!
//! Position in the compile pipeline:
//! ```text
//! BF source → Token → AST → HIR → LIR → [AsmProgram] → machine code → executable
//!                                        ^^^^^^^^^^
//!                                        defined here
//! ```
//!
//! `AsmProgram` is a flat list of `AsmInst` values, each corresponding to a
//! single x86_64 instruction (or a label pseudo-instruction). This layer lets:
//! - `codegen.rs` stay free of machine-code encoding details,
//! - `encode.rs` stay free of Brainfuck semantics and register allocation, and
//! - `debug.rs` format / analyse an `AsmProgram` independently.
//!
//! ## Register convention (assigned by `codegen.rs`)
//!
//! | Reg | Purpose                                                          |
//! |-----|------------------------------------------------------------------|
//! | R12 | Tape base address (as returned by mmap / VirtualAlloc)           |
//! | R13 | Current data pointer (the Brainfuck cell pointer)                |
//! | R14 | Tape end address (base + length)                                 |
//! | R15 | Candidate target for `PtrAdd` prior to bounds checking           |
//! | RAX | Syscall number / return value                                    |
//! | RSP | Stack pointer (Win64 shadow space + scratch slots)               |
//! | RDI | Syscall arg 1                                                    |
//! | RSI | Syscall arg 2                                                    |
//! | RDX | Syscall arg 3 / scratch                                          |
//! | R10 | Syscall arg 4 / scratch (`old_len`)                              |
//! | R8  | Syscall arg 5 / scratch (`copy_start`)                           |
//! | R9  | Syscall arg 6 / scratch (`desired_offset`)                       |
//! | R11 | Scratch (`new_len`; clobbered by `syscall`)                      |
//! | RCX | `rep movsb` counter (clobbered by `syscall`)                     |

use std::fmt;

/// Assembly label, marking a jump / call target inside the program.
///
/// The wrapped `u32` is a unique identifier partitioned into two ranges so
/// user-supplied labels and compiler-synthesised labels cannot collide:
/// - User labels (forwarded from LIR `LabelId`) start at `0` and count up.
/// - Internal helper labels count down from `u32::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AsmLabel(pub u32);

/// x86_64 general-purpose 64-bit registers actually used by this backend.
///
/// The callee-saved `Rbx` / `Rbp` hold the output-buffer write pointer and
/// end sentinel respectively for the D3 buffered-stdio path; every other
/// register is reused across the Linux / Windows ABIs in the way described
/// at the top of this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg64 {
    /// Accumulator / syscall number / syscall return value.
    Rax,
    /// Counter (also the `rep` count register; clobbered by `syscall`).
    Rcx,
    /// Syscall arg 3 / general scratch.
    Rdx,
    /// Output-buffer write pointer (callee-saved; preserved across syscalls).
    Rbx,
    /// Stack pointer.
    Rsp,
    /// Output-buffer end sentinel = buffer_base + 4096 (callee-saved).
    Rbp,
    /// Source pointer for `rep movsb` / syscall arg 2.
    Rsi,
    /// Destination pointer for `rep movsb` / syscall arg 1.
    Rdi,
    /// Syscall arg 5 / scratch.
    R8,
    /// Syscall arg 6 / scratch.
    R9,
    /// Syscall arg 4 / scratch.
    R10,
    /// Scratch (clobbered by `syscall`, which preserves the old RFLAGS here).
    R11,
    /// Tape base address (set once at program start).
    R12,
    /// Current data pointer (the Brainfuck cell pointer).
    R13,
    /// Tape end address (`tape_base + tape_len`).
    R14,
    /// Candidate target for `PtrAdd` prior to bounds checking.
    R15,
}

/// Renders a `Reg64` as its lowercase mnemonic (e.g. `"rax"`).
///
/// Enables using `{}` in format strings for instruction pretty-printing, e.g.
/// `format!("mov {}, {}", dst, src)` → `"mov rax, rcx"`.
impl fmt::Display for Reg64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Reg64::Rax => "rax",
            Reg64::Rcx => "rcx",
            Reg64::Rdx => "rdx",
            Reg64::Rbx => "rbx",
            Reg64::Rsp => "rsp",
            Reg64::Rbp => "rbp",
            Reg64::Rsi => "rsi",
            Reg64::Rdi => "rdi",
            Reg64::R8 => "r8",
            Reg64::R9 => "r9",
            Reg64::R10 => "r10",
            Reg64::R11 => "r11",
            Reg64::R12 => "r12",
            Reg64::R13 => "r13",
            Reg64::R14 => "r14",
            Reg64::R15 => "r15",
        };
        write!(f, "{}", name)
    }
}

/// Renders an `AsmLabel` as `L<id>` (e.g. `L0`, `L42`).
impl fmt::Display for AsmLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}", self.0)
    }
}

/// x86_64 assembly instruction.
///
/// Each variant maps to a single x86_64 instruction (or the `Label`
/// pseudo-instruction). The set is intentionally the minimum subset that the
/// Brainfuck compiler actually emits; rough groupings:
///
/// 1. Pseudo: `Label`.
/// 2. Data movement: `MovRegImm64`, `MovRegReg`, `MovMem8Imm8`.
/// 3. Arithmetic: `AddRegImm32`, `AndRegImm32`, `AddRegReg`, `SubRegReg`,
///    `AddMem8Imm8`, `IncMem8`, `DecMem8`.
/// 4. Comparison: `CmpRegReg`, `CmpRegImm32`, `CmpMem8Imm8`.
/// 5. Addressing / stack slots / RIP-relative: `LeaRegMem`, `LeaRegLabel`,
///    `MovMemReg64`, `MovRegMem64`.
/// 6. Shifts: `ShrRegImm8`.
/// 7. Control flow: `Jz`, `Jnz`, `Jb`, `Jae`, `Jl`, `Jge`, `Jmp` (rel32
///    forms), plus the `*Short` rel8 counterparts produced by branch
///    relaxation; `Call`, `CallMemLabel`, `Ret`.
/// 8. String ops: `Cld`, `RepMovsb`.
/// 9. System call: `Syscall`.
/// 10. Raw machine code: `RawBytes` (used by `-O3` pre-assembled paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmInst {
    /// Label definition (pseudo-instruction; emits no machine bytes).
    ///
    /// Marks a code position referenced by jumps and calls.
    Label(AsmLabel),

    /// `mov reg, imm64` — load a 64-bit immediate into a register.
    ///
    /// The only x86_64 form that can load a full 64-bit value directly; the
    /// encoding is 10 bytes (REX + opcode + 8-byte immediate).
    MovRegImm64(Reg64, i64),

    /// `mov dst, src` — register-to-register move.
    MovRegReg(Reg64, Reg64),

    /// `add reg, imm32` — add a sign-extended 32-bit immediate to a register.
    ///
    /// The immediate is sign-extended to 64 bits before the add; used for
    /// pointer-offset arithmetic (tape base + offset).
    AddRegImm32(Reg64, i32),

    /// `and reg, imm32` — bitwise AND with a sign-extended 32-bit immediate.
    ///
    /// Mainly used for stack-pointer alignment, e.g. `and rsp, -16`.
    AndRegImm32(Reg64, i32),

    /// `add dst, src` — register + register.
    AddRegReg(Reg64, Reg64),

    /// `sub dst, src` — register - register.
    SubRegReg(Reg64, Reg64),

    /// `cmp lhs, rhs` — register compare (updates EFLAGS, discards result).
    ///
    /// Logically `lhs - rhs` with the difference thrown away; only EFLAGS is
    /// updated.
    CmpRegReg(Reg64, Reg64),

    /// `cmp reg, imm32` — register vs. 32-bit immediate compare.
    CmpRegImm32(Reg64, i32),

    /// `shr reg, imm8` — logical right shift by an 8-bit immediate count.
    ///
    /// Used during tape growth to compute `(new_len - old_len) / 2`.
    ShrRegImm8(Reg64, u8),

    /// `lea dst, [base + disp]` — compute `base + disp` as an address.
    ///
    /// Used when building Win64 scratch-slot / fifth-argument addresses.
    LeaRegMem(Reg64, Reg64, i32),

    /// `lea dst, [rip + rel32]` — take the address of a nearby label.
    ///
    /// Used to reach IAT entries, embedded strings, and read-only data placed
    /// in the same section.
    LeaRegLabel(Reg64, AsmLabel),

    /// `mov qword ptr [base + disp], src` — store a 64-bit register to memory.
    ///
    /// Primarily used for Win64 shadow space and stack-slot writes.
    MovMemReg64(Reg64, i32, Reg64),

    /// `mov dst, qword ptr [base + disp]` — load a 64-bit value from memory.
    ///
    /// Primarily used for Win64 shadow space and stack-slot reads.
    MovRegMem64(Reg64, Reg64, i32),

    /// `add byte ptr [reg], imm8` — add an 8-bit immediate to a memory byte.
    ///
    /// Adds to the single byte at the address in `reg`; this is the direct
    /// lowering of Brainfuck `+` / `-`. The encoder only accepts base-register
    /// forms that do not need a SIB byte (codegen hard-pins this to R13).
    AddMem8Imm8(Reg64, i8),

    /// `inc byte ptr [reg]` — increment a memory byte by one.
    ///
    /// Short form of `add byte [reg], 1` (4 bytes vs. 5). Same SIB-free
    /// restriction as [`AsmInst::AddMem8Imm8`].
    IncMem8(Reg64),

    /// `dec byte ptr [reg]` — decrement a memory byte by one.
    ///
    /// Short form of `add byte [reg], -1` (4 bytes vs. 5). Same SIB-free
    /// restriction as [`AsmInst::AddMem8Imm8`].
    DecMem8(Reg64),

    /// `mov byte ptr [reg], imm8` — store an 8-bit immediate into memory.
    ///
    /// Used for optimised BF patterns, e.g. `[-]` being folded into
    /// `CellSet(0)`. Same SIB-free restriction as `AddMem8Imm8`.
    MovMem8Imm8(Reg64, u8),

    /// `add byte ptr [reg + disp8], imm8` — add an 8-bit immediate to the
    /// byte at `reg + signed-disp8`.
    ///
    /// Produced by the LIR `lir_postpone` pass (B4 / C3). `disp` is the
    /// signed 8-bit displacement; `imm` is the signed 8-bit immediate. Same
    /// SIB-free restriction as [`AsmInst::AddMem8Imm8`] — codegen hard-pins
    /// the base register to R13.
    AddMem8ImmDisp8(Reg64, i8, i8),

    /// `mov byte ptr [reg + disp8], imm8` — store an 8-bit immediate at
    /// `reg + signed-disp8`.
    ///
    /// Produced by the LIR `lir_postpone` pass (B4 / C3). Same SIB-free
    /// restriction as [`AsmInst::MovMem8Imm8`].
    MovMem8ImmDisp8(Reg64, i8, u8),

    /// `add byte ptr [reg + disp32], imm8` — add an 8-bit immediate to the
    /// byte at `reg + signed-disp32`.
    ///
    /// Disp32 counterpart of [`AsmInst::AddMem8ImmDisp8`], selected by
    /// codegen when the offset does not fit in `i8`. Same SIB-free
    /// restriction as [`AsmInst::AddMem8Imm8`].
    AddMem8ImmDisp32(Reg64, i32, i8),

    /// `mov byte ptr [reg + disp32], imm8` — store an 8-bit immediate at
    /// `reg + signed-disp32`.
    ///
    /// Disp32 counterpart of [`AsmInst::MovMem8ImmDisp8`]. Same SIB-free
    /// restriction as [`AsmInst::MovMem8Imm8`].
    MovMem8ImmDisp32(Reg64, i32, u8),

    /// `cmp byte ptr [reg], imm8` — compare a memory byte with an immediate.
    ///
    /// Used by BF `[` / `]` to decide whether the current cell is zero. Same
    /// SIB-free restriction as `AddMem8Imm8`.
    CmpMem8Imm8(Reg64, u8),

    /// `jz label` — conditional jump when ZF=1 (result was zero).
    ///
    /// Used by BF `[`: skip the loop body when the current cell is zero.
    /// Encoded as the 6-byte `0F 84 rel32` form; the relax pass narrows
    /// in-range variants to [`AsmInst::JzShort`].
    Jz(AsmLabel),

    /// `jnz label` — conditional jump when ZF=0 (result was non-zero).
    ///
    /// Used by BF `]`: jump back to the loop head when the current cell is
    /// non-zero. Narrowed by the relax pass to [`AsmInst::JnzShort`] where the
    /// target sits within rel8 range.
    Jnz(AsmLabel),

    /// `jb label` — unsigned branch when CF=1 (below).
    ///
    /// Used by tape bounds-checking: pointer < tape base. Narrowed by the
    /// relax pass to [`AsmInst::JbShort`] where in-range.
    Jb(AsmLabel),

    /// `jae label` — unsigned branch when CF=0 (above or equal).
    ///
    /// Used by tape bounds-checking: pointer >= tape end. Narrowed by the
    /// relax pass to [`AsmInst::JaeShort`] where in-range.
    Jae(AsmLabel),

    /// `jl label` — signed branch when SF≠OF (less than).
    ///
    /// Used to detect a negative mmap return value (an error). Narrowed by
    /// the relax pass to [`AsmInst::JlShort`] where in-range.
    Jl(AsmLabel),

    /// `jge label` — signed branch when SF=OF (greater than or equal).
    ///
    /// Used to terminate the tape-growth loop. Narrowed by the relax pass to
    /// [`AsmInst::JgeShort`] where in-range.
    Jge(AsmLabel),

    /// `jmp label` — unconditional jump.
    ///
    /// Encoded as the 5-byte `E9 rel32` form; narrowed by the relax pass to
    /// [`AsmInst::JmpShort`] where the target sits within rel8 range.
    Jmp(AsmLabel),

    /// `jz label` (short form, 2 bytes: `74 rel8`).
    ///
    /// Produced only by the branch-relaxation pass when the target fits the
    /// signed 8-bit range measured from the instruction that follows this
    /// one.
    JzShort(AsmLabel),

    /// `jnz label` (short form, 2 bytes: `75 rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JnzShort(AsmLabel),

    /// `jb label` (short form, 2 bytes: `72 rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JbShort(AsmLabel),

    /// `jae label` (short form, 2 bytes: `73 rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JaeShort(AsmLabel),

    /// `jl label` (short form, 2 bytes: `7C rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JlShort(AsmLabel),

    /// `jge label` (short form, 2 bytes: `7D rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JgeShort(AsmLabel),

    /// `jmp label` (short form, 2 bytes: `EB rel8`).
    ///
    /// Produced only by the branch-relaxation pass; same rel8 constraint as
    /// [`AsmInst::JzShort`].
    JmpShort(AsmLabel),

    /// `call label` — function call.
    ///
    /// Pushes the return address (RIP of the following instruction) onto the
    /// stack, then jumps to `label`. Used to call the `ensure_tape` grow
    /// routine.
    Call(AsmLabel),

    /// `call qword ptr [rip + rel32]` — indirect call through a memory slot.
    ///
    /// Primarily used to call Win32 APIs via the PE import table.
    CallMemLabel(AsmLabel),

    /// `ret` — return from a function.
    ///
    /// Pops the return address from the top of the stack and jumps there.
    Ret,

    /// `cld` — clear the direction flag (DF=0).
    ///
    /// Ensures a following `rep movsb` copies forward (auto-incrementing).
    Cld,

    /// `rep movsb` — repeat byte move.
    ///
    /// Copies `rcx` bytes from `[rsi]` to `[rdi]`, incrementing both pointers
    /// by 1 per iteration (DF=0 guaranteed by `Cld`). Used during tape growth
    /// to move old contents into the new buffer.
    RepMovsb,

    /// `std` — set the direction flag (DF=1).
    ///
    /// Used by `Scan(-1)` lowering so the following `repne scasb` walks
    /// backwards. Must be paired with a `Cld` before any function-boundary
    /// `call` / `ret` (Win64 + SysV both require DF=0 at boundaries).
    Std,

    /// `repne scasb` — repeat-not-equal byte scan.
    ///
    /// While `rcx > 0`, compares `al` against `[rdi]` and post-{inc,dec}rements
    /// `rdi` (sign of step set by DF: forward when `Cld`, backward when `Std`).
    /// Stops on the first match (ZF=1) or when `rcx` underflows. The D2 SIMD
    /// fast path for `[<]` / `[>]` searches the tape for `al = 0`.
    RepneScasb,

    /// `xor eax, eax` — zero RAX (the upper 32 bits are also cleared).
    ///
    /// 2 bytes; replaces the 10-byte `mov rax, 0` when only the value 0 is
    /// needed. Used by D2's `rep stosb` setup (the comparand byte is `al`)
    /// and `repne scasb` setup.
    XorEaxEax,

    /// `mov ecx, imm32` — load a 32-bit immediate into ECX (zero-extended into RCX).
    ///
    /// 5 bytes (`B9 + imm32`). Distinct from `MovRegImm64` so the encoder can
    /// pick the shorter form when `rcx` only needs a 32-bit count (e.g. D2's
    /// `rep stosb` over a `ZeroRun`).
    MovEcxImm32(i32),

    /// `rep stosb` — repeat byte store.
    ///
    /// While `rcx > 0`, stores `al` into `[rdi]` and post-{inc,dec}rements
    /// `rdi` (DF). The D2 SIMD path uses it to zero a `ZeroRun(count >= 16)`
    /// in a single hardware-driven loop.
    RepStosb,

    /// `syscall` — raise a Linux x86_64 system call.
    ///
    /// Calling convention:
    /// - `rax` = syscall number
    /// - `rdi, rsi, rdx, r10, r8, r9` = args 1..=6
    /// - return value written back to `rax`
    /// - kernel clobbers `rcx` (saved RIP) and `r11` (saved RFLAGS)
    Syscall,

    /// Raw machine-code bytes, pre-assembled by `-O3` paths that bypass the
    /// per-instruction encoder.
    RawBytes(Vec<u8>),

    /// `push r64` — push a 64-bit register onto the stack.
    Push(Reg64),

    /// `pop r64` — pop into a 64-bit register from the stack.
    Pop(Reg64),

    /// `movzx ebx, byte [r13]` — read the current tape cell into `ebx`'s low
    /// byte, zero-extending the upper bits.
    MovzxEbxFromMemR13,

    /// `mov eax, ebx` — copy `ebx` to `eax` (upper 32 bits of `rax` cleared).
    MovEaxEbx,

    /// `imul eax, ebx, imm32` — multiply `ebx` by an immediate into `eax`.
    ///
    /// Used only by `-O1 LinearMul`; the low 8 bits of the result match tape
    /// semantics exactly.
    ImulEaxEbxImm32(i32),

    /// `add byte [r13], al` — add `al` into the current tape cell.
    AddMemR13Al,

    /// `mov al, byte [r13]` — load the current tape cell into `al`.
    ///
    /// Used by the buffered-stdout `PutByte` emit: reads `*data_ptr` into
    /// `al` in preparation for `mov [rbx], al`.  R13 encodes with
    /// `mod=01 + disp8=0` to dodge the `[RIP+disp32]` aliasing that
    /// `mod=00 + rm&7==5` would otherwise trigger.
    MovAlMemR13,

    /// `mov byte [rbx], al` — store `al` into the output buffer at the
    /// current write pointer.
    ///
    /// Paired with a following `add rbx, 1` + `cmp rbx, rbp` so the buffer
    /// flushes when it hits `rbp` (the precomputed end-of-buffer address).
    MovMemRbxAl,
}

/// Assembly program: a flat sequence of instructions.
///
/// The output of `codegen` and the input to `encode`. All jump targets are
/// referenced via `AsmLabel`; the `encode` stage resolves them into relative
/// offsets.
#[derive(Debug, Clone)]
pub struct AsmProgram {
    /// Instructions in emission order.
    pub insts: Vec<AsmInst>,
}
