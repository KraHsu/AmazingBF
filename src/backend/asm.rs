#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AsmLabel(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg64 {
    Rax,
    Rdi,
    Rsi,
    Rdx,
    R13,
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

    Syscall,
}

#[derive(Debug, Clone)]
pub struct AsmProgram {
    pub insts: Vec<AsmInst>,
    pub tape_label: AsmLabel,
    pub tape_size: usize,
}
