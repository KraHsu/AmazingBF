//! # 汇编 IR 定义 (asm.rs)
//!
//! 本模块定义了 x86_64 后端的**汇编中间表示**（Assembly IR）。
//!
//! 在编译流水线中的位置：
//! ```text
//! BF 源码 → Token → AST → HIR → LIR → [AsmProgram] → 机器码 → ELF
//!                                      ^^^^^^^^^^
//!                                      本模块定义
//! ```
//!
//! `AsmProgram` 是一个平坦的指令列表，每条 `AsmInst` 对应一条 x86_64 指令
//! （或一个标签伪指令）。这一层抽象使得：
//! - codegen.rs 不需要关心具体的机器码编码细节
//! - encode.rs 不需要关心 BF 语义和寄存器分配策略
//! - debug.rs 可以独立地对 AsmProgram 进行格式化和分析
//!
//! ## 寄存器约定（由 codegen.rs 定义）
//!
//! | 寄存器 | 用途                                         |
//! |--------|----------------------------------------------|
//! | R12    | tape 基址（mmap 返回的起始地址）              |
//! | R13    | 当前指针位置（BF 的 data pointer）            |
//! | R14    | tape 结束地址（base + length）                |
//! | R15    | PtrAdd 的临时目标地址（边界检查前的候选值）   |
//! | RAX    | 系统调用号 / 返回值                          |
//! | RDI    | 系统调用参数 1                               |
//! | RSI    | 系统调用参数 2                               |
//! | RDX    | 系统调用参数 3 / 临时变量                    |
//! | R10    | 系统调用参数 4 / 临时变量（old_len）         |
//! | R8     | 系统调用参数 5 / 临时变量（copy_start）      |
//! | R9     | 系统调用参数 6 / 临时变量（desired_offset）  |
//! | R11    | 临时变量（new_len，注意 syscall 会覆写此寄存器）|
//! | RCX    | rep movsb 的计数器（注意 syscall 会覆写此寄存器）|

use std::fmt;

/// 汇编标签，用于标记代码中的跳转目标。
///
/// 内部使用 `u32` 作为唯一标识符：
/// - 用户标签（来自 LIR 的 LabelId）使用低位值（0, 1, 2, ...）
/// - 内部标签（编译器生成的辅助标签）使用高位值（从 u32::MAX 递减）
///
/// 两类标签的 ID 空间不重叠，因此不会冲突。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AsmLabel(pub u32);

/// x86_64 通用 64 位寄存器。
///
/// 仅列出本编译器实际使用的寄存器，未包含 RBX、RSP、RBP。
/// 这是因为：
/// - RSP/RBP 用于栈管理，本编译器不使用栈帧
/// - RBX 是 callee-saved 寄存器，保留以备将来扩展
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

/// 为 `Reg64` 实现 `Display` trait，输出小写寄存器名。
///
/// 这使得在格式化字符串中可以直接使用 `{}` 占位符输出寄存器名称，
/// 例如 `format!("mov {}, {}", dst, src)` → `"mov rax, rcx"`。
impl fmt::Display for Reg64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Reg64::Rax => "rax",
            Reg64::Rcx => "rcx",
            Reg64::Rdx => "rdx",
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

/// 为 `AsmLabel` 实现 `Display` trait，输出标签名。
impl fmt::Display for AsmLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}", self.0)
    }
}

/// x86_64 汇编指令枚举。
///
/// 每个变体对应一条具体的 x86_64 指令（或标签伪指令）。
/// 指令集被限制为 Brainfuck 编译器实际需要的最小子集。
///
/// 指令按功能分组：
/// 1. 伪指令（Label）
/// 2. 数据传送（MovRegImm64, MovRegReg, MovMem8Imm8）
/// 3. 算术运算（AddRegImm32, AddRegReg, SubRegReg, AddMem8Imm8）
/// 4. 比较（CmpRegReg, CmpRegImm32, CmpMem8Imm8）
/// 5. 移位（ShrRegImm8）
/// 6. 控制流（Jz, Jnz, Jb, Jae, Jl, Jge, Jmp, Call, Ret）
/// 7. 字符串操作（Cld, RepMovsb）
/// 8. 系统调用（Syscall）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmInst {
    /// 标签定义（伪指令，不产生机器码字节）。
    ///
    /// 标记一个代码位置，供跳转和调用指令引用。
    Label(AsmLabel),

    /// `mov reg, imm64` — 将 64 位立即数加载到寄存器。
    ///
    /// 这是 x86_64 中唯一能直接加载 64 位值的指令，
    /// 编码长度为 10 字节（REX + opcode + 8 字节立即数）。
    MovRegImm64(Reg64, i64),

    /// `mov dst, src` — 寄存器间数据传送。
    MovRegReg(Reg64, Reg64),

    /// `add reg, imm32` — 寄存器加符号扩展的 32 位立即数。
    ///
    /// imm32 会被符号扩展到 64 位后再相加。
    /// 用于指针偏移计算（如 tape 基址 + 偏移量）。
    AddRegImm32(Reg64, i32),

    /// `add dst, src` — 寄存器加寄存器。
    AddRegReg(Reg64, Reg64),

    /// `sub dst, src` — 寄存器减寄存器。
    SubRegReg(Reg64, Reg64),

    /// `cmp lhs, rhs` — 寄存器比较（设置标志位，不保存结果）。
    ///
    /// 等价于 `lhs - rhs`，但结果被丢弃，仅更新 EFLAGS。
    CmpRegReg(Reg64, Reg64),

    /// `cmp reg, imm32` — 寄存器与立即数比较。
    CmpRegImm32(Reg64, i32),

    /// `shr reg, imm8` — 逻辑右移指定位数。
    ///
    /// 用于 tape 扩容时计算 `(new_len - old_len) / 2`。
    ShrRegImm8(Reg64, u8),

    /// `add byte ptr [reg], imm8` — 内存字节加立即数。
    ///
    /// 对 `reg` 指向的内存地址处的单个字节进行加法。
    /// 这是 Brainfuck `+` 和 `-` 指令的直接实现。
    /// 当前编码器仅支持无需 SIB 的基址寄存器组合（实际由 codegen 固定为 R13）。
    AddMem8Imm8(Reg64, i8),

    /// `mov byte ptr [reg], imm8` — 将立即数写入内存字节。
    ///
    /// 用于优化后的 BF 操作，如 `[-]` 被优化为 `CellSet(0)`。
    /// 当前编码器仅支持无需 SIB 的基址寄存器组合（实际由 codegen 固定为 R13）。
    MovMem8Imm8(Reg64, u8),

    /// `cmp byte ptr [reg], imm8` — 内存字节与立即数比较。
    ///
    /// 用于 BF 的 `[` 和 `]`：检查当前单元是否为零。
    /// 当前编码器仅支持无需 SIB 的基址寄存器组合（实际由 codegen 固定为 R13）。
    CmpMem8Imm8(Reg64, u8),

    /// `jz label` — 条件跳转：零标志位为 1 时跳转（ZF=1）。
    ///
    /// 用于 BF 的 `[`：当前单元为零时跳过循环体。
    Jz(AsmLabel),

    /// `jnz label` — 条件跳转：零标志位为 0 时跳转（ZF=0）。
    ///
    /// 用于 BF 的 `]`：当前单元非零时跳回循环开头。
    Jnz(AsmLabel),

    /// `jb label` — 无符号条件跳转：低于时跳转（CF=1）。
    ///
    /// 用于 tape 边界检查：指针 < tape 基址。
    Jb(AsmLabel),

    /// `jae label` — 无符号条件跳转：高于等于时跳转（CF=0）。
    ///
    /// 用于 tape 边界检查：指针 >= tape 结束地址。
    Jae(AsmLabel),

    /// `jl label` — 有符号条件跳转：小于时跳转（SF≠OF）。
    ///
    /// 用于检查 mmap 返回值是否为负（表示错误）。
    Jl(AsmLabel),

    /// `jge label` — 有符号条件跳转：大于等于时跳转（SF=OF）。
    ///
    /// 用于 tape 扩容循环的终止条件。
    Jge(AsmLabel),

    /// `jmp label` — 无条件跳转。
    Jmp(AsmLabel),

    /// `call label` — 函数调用。
    ///
    /// 将返回地址（下一条指令的 RIP）压入栈中，然后跳转到目标标签。
    /// 用于调用 `ensure_tape` 扩容函数。
    Call(AsmLabel),

    /// `ret` — 函数返回。
    ///
    /// 从栈顶弹出返回地址并跳转回去。
    Ret,

    /// `cld` — 清除方向标志位（DF=0）。
    ///
    /// 确保后续的 `rep movsb` 向前（递增方向）复制数据。
    Cld,

    /// `rep movsb` — 重复字节复制。
    ///
    /// 将 `rcx` 个字节从 `[rsi]` 复制到 `[rdi]`，
    /// 每次复制后 rsi 和 rdi 各递增 1（因为 DF=0）。
    /// 用于 tape 扩容时将旧数据复制到新缓冲区。
    RepMovsb,

    /// `syscall` — 触发 Linux x86_64 系统调用。
    ///
    /// 系统调用约定：
    /// - rax = 系统调用号
    /// - rdi, rsi, rdx, r10, r8, r9 = 参数 1~6
    /// - 返回值写入 rax
    /// - 内核会覆写 rcx（保存旧 RIP）和 r11（保存旧 RFLAGS）
    Syscall,
}

/// 汇编程序：一个平坦的指令序列。
///
/// 这是 codegen 的输出、encode 的输入。
/// 所有跳转目标通过 `AsmLabel` 引用，由 encode 阶段解析为相对偏移。
#[derive(Debug, Clone)]
pub struct AsmProgram {
    pub insts: Vec<AsmInst>,
}
