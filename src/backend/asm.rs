#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AsmLabel(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg64 {
    Rax,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmInst {
    Label(AsmLabel),

    /// lea reg, [rip + label]
    LeaRipLabel(Reg64, AsmLabel),

    /// mov reg, imm64
    MovRegImm64(Reg64, i64),

    /// mov dst, src
    MovRegReg(Reg64, Reg64),

    /// add reg, imm32
    AddRegImm32(Reg64, i32),

    /// add dst, src
    AddRegReg(Reg64, Reg64),

    /// sub dst, src
    SubRegReg(Reg64, Reg64),

    /// cmp lhs, rhs
    CmpRegReg(Reg64, Reg64),

    /// cmp reg, imm32
    CmpRegImm32(Reg64, i32),

    /// shr reg, imm8
    ShrRegImm8(Reg64, u8),

    /// add byte ptr [reg], imm8
    AddMem8Imm8(Reg64, i8),

    /// mov byte ptr [reg], imm8
    MovMem8Imm8(Reg64, u8),

    /// cmp byte ptr [reg], imm8
    CmpMem8Imm8(Reg64, u8),

    /// jump if zero label
    Jz(AsmLabel),

    /// jump if not zero label
    Jnz(AsmLabel),

    /// unsigned: jump if below
    Jb(AsmLabel),

    /// unsigned: jump if above or equal
    Jae(AsmLabel),

    /// signed: jump if less than
    Jl(AsmLabel),

    /// signed: jump if greater or equal
    Jge(AsmLabel),

    /// unconditional jump
    Jmp(AsmLabel),

    /// call label
    Call(AsmLabel),

    /// ret
    Ret,

    /// clear direction flag
    Cld,

    /// rep movsb
    RepMovsb,

    Syscall,
}

#[derive(Debug, Clone)]
pub struct AsmProgram {
    pub insts: Vec<AsmInst>,
}
