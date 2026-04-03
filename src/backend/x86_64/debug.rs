//! # 调试输出模块 (debug.rs)
//!
//! 本模块为 Brainfuck 编译器的 x86_64 后端提供两种调试输出能力：
//!
//! - **方案 A**：`dump_asm_listing` —— 将 `AsmProgram` 转换为人类可读的汇编文本，
//!   类似于 nasm/gas 风格的输出，用于快速查看编译器生成了哪些指令。
//!
//! - **方案 B**：`dump_hex_listing` —— 将 `AsmProgram` 编码为机器码，同时生成
//!   带偏移量的 hex dump，每行显示"偏移 : 字节序列 : 汇编助记符"，
//!   用于逐字节核对编码是否正确。
//!
//! 两者配合使用，可以在不依赖 objdump/gdb 的情况下快速定位编译器 bug。
//!
//! ## 使用示例
//!
//! ```rust,ignore
//! use crate::backend::debug;
//!
//! let asm_program = compile_lir_to_asm(&lir);
//!
//! // 方案 A：输出汇编文本
//! let asm_text = debug::dump_asm_listing(&asm_program);
//! std::fs::write("output.asm", &asm_text)?;
//!
//! // 方案 B：输出带 hex 的 listing
//! let hex_text = debug::dump_hex_listing(&asm_program);
//! std::fs::write("output.lst", &hex_text)?;
//! ```

use std::collections::HashMap;
use std::fmt::Write;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};

// ============================================================================
// 辅助：寄存器名称格式化
// ============================================================================

/// 将 `Reg64` 枚举值转换为小写的 x86_64 寄存器名称字符串。
///
/// 例如 `Reg64::Rax` → `"rax"`，`Reg64::R13` → `"r13"`。
fn reg_name(reg: Reg64) -> &'static str {
    match reg {
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
    }
}

/// 将标签 ID 格式化为统一的标签名称字符串。
///
/// 内部标签（高位 u32 值）会被赋予可读的名称，例如：
/// - `u32::MAX`     → `"__ensure_tape"`
/// - `u32::MAX - 1` → `"__oom_exit"`
/// - 其他高位值    → `"__internal_XXXXXXXX"`（十六进制）
/// - 普通用户标签  → `"L0"`, `"L1"`, ...
fn label_name(label: AsmLabel) -> String {
    // 这些常量与 codegen.rs 中的定义保持一致
    const INTERNAL_LABEL_ENSURE_TAPE_RAW: u32 = u32::MAX;
    const INTERNAL_LABEL_OOM_EXIT_RAW: u32 = u32::MAX - 1;
    const INTERNAL_LABEL_GROW_LOOP_RAW: u32 = u32::MAX - 2;
    const INTERNAL_LABEL_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_GROW_LOOP_RAW;

    match label.0 {
        INTERNAL_LABEL_ENSURE_TAPE_RAW => "__ensure_tape".to_string(),
        INTERNAL_LABEL_OOM_EXIT_RAW => "__oom_exit".to_string(),
        INTERNAL_LABEL_GROW_LOOP_RAW => "__grow_loop".to_string(),
        // 固定语义的内部标签已经在上面单独匹配。
        // 剩余高位编号视为临时内部标签。
        raw if raw >= 0xFFFF_0000 && raw < INTERNAL_LABEL_RESERVED_MIN_RAW => {
            format!("__internal_{:08x}", raw)
        }
        // 普通用户标签（来自 LIR 的 LabelId）
        raw => format!("L{}", raw),
    }
}

// ============================================================================
// 方案 A：汇编文本 Listing
// ============================================================================

/// 【方案 A】将 `AsmProgram` 转换为人类可读的汇编文本。
///
/// 输出格式类似于 nasm/gas 风格，例如：
/// ```text
/// ; === Brainfuck x86_64 Assembly Listing ===
/// ; 共 42 条指令
///
/// __ensure_tape:
///     mov     rax, 0x9                    ; syscall: mmap
///     mov     rdi, 0x0
///     ...
/// ```
///
/// # 参数
/// - `program`: 要转换的汇编程序
///
/// # 返回值
/// 格式化后的汇编文本字符串
pub fn dump_asm_listing(program: &AsmProgram) -> String {
    let mut out = String::new();

    // ---- 文件头部注释 ----
    writeln!(out, "; === Brainfuck x86_64 Assembly Listing ===").unwrap();
    writeln!(out, "; 共 {} 条指令（含标签伪指令）", program.insts.len()).unwrap();
    writeln!(out).unwrap();

    // ---- 逐条格式化 ----
    for inst in &program.insts {
        format_inst_asm(&mut out, inst);
    }

    out
}

/// 将单条 `AsmInst` 格式化为汇编文本，写入 `out`。
///
/// 标签不缩进（顶格），普通指令缩进 4 个空格。
fn format_inst_asm(out: &mut String, inst: &AsmInst) {
    match inst {
        // ---- 伪指令：标签定义 ----
        AsmInst::Label(label) => {
            // 标签前空一行以提高可读性
            writeln!(out, "{}:", label_name(*label)).unwrap();
        }

        // ---- 地址加载 ----
        AsmInst::LeaRipLabel(reg, label) => {
            writeln!(
                out,
                "    lea     {}, [rip + {}]",
                reg_name(*reg),
                label_name(*label)
            )
            .unwrap();
        }

        // ---- 数据移动 ----
        AsmInst::MovRegImm64(reg, imm) => {
            // 对于常见的系统调用号，添加注释说明
            let comment = match imm {
                0 => " ; sys_read",
                1 => " ; sys_write",
                9 => " ; sys_mmap",
                11 => " ; sys_munmap",
                60 => " ; sys_exit",
                _ => "",
            };
            writeln!(
                out,
                "    mov     {}, 0x{:x}{}",
                reg_name(*reg),
                *imm as u64, // 以无符号十六进制显示
                comment
            )
            .unwrap();
        }

        AsmInst::MovRegReg(dst, src) => {
            writeln!(out, "    mov     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        // ---- 算术运算 ----
        AsmInst::AddRegImm32(reg, imm) => {
            writeln!(out, "    add     {}, 0x{:x}", reg_name(*reg), *imm as u32).unwrap();
        }

        AsmInst::AddRegReg(dst, src) => {
            writeln!(out, "    add     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        AsmInst::SubRegReg(dst, src) => {
            writeln!(out, "    sub     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        // ---- 比较 ----
        AsmInst::CmpRegReg(lhs, rhs) => {
            writeln!(out, "    cmp     {}, {}", reg_name(*lhs), reg_name(*rhs)).unwrap();
        }

        AsmInst::CmpRegImm32(reg, imm) => {
            writeln!(out, "    cmp     {}, 0x{:x}", reg_name(*reg), *imm as u32).unwrap();
        }

        // ---- 移位 ----
        AsmInst::ShrRegImm8(reg, imm) => {
            writeln!(out, "    shr     {}, {}", reg_name(*reg), imm).unwrap();
        }

        // ---- 内存字节操作（Brainfuck 核心） ----
        AsmInst::AddMem8Imm8(reg, imm) => {
            // 这是 BF 的 '+'/'-' 操作：修改当前单元的值
            writeln!(
                out,
                "    add     byte [{}], 0x{:02x}",
                reg_name(*reg),
                *imm as u8
            )
            .unwrap();
        }

        AsmInst::MovMem8Imm8(reg, imm) => {
            // 这是优化后的 BF 操作：直接设置单元值（如 [-] 优化为 set 0）
            writeln!(out, "    mov     byte [{}], 0x{:02x}", reg_name(*reg), imm).unwrap();
        }

        AsmInst::CmpMem8Imm8(reg, imm) => {
            // 用于 BF 的 '[' 和 ']'：比较当前单元是否为零
            writeln!(out, "    cmp     byte [{}], 0x{:02x}", reg_name(*reg), imm).unwrap();
        }

        // ---- 条件跳转 ----
        AsmInst::Jz(label) => {
            writeln!(out, "    jz      {}", label_name(*label)).unwrap();
        }

        AsmInst::Jnz(label) => {
            writeln!(out, "    jnz     {}", label_name(*label)).unwrap();
        }

        AsmInst::Jb(label) => {
            writeln!(
                out,
                "    jb      {}           ; unsigned below",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jae(label) => {
            writeln!(
                out,
                "    jae     {}           ; unsigned above or equal",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jl(label) => {
            writeln!(
                out,
                "    jl      {}           ; signed less",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jge(label) => {
            writeln!(
                out,
                "    jge     {}           ; signed greater or equal",
                label_name(*label)
            )
            .unwrap();
        }

        // ---- 无条件跳转 ----
        AsmInst::Jmp(label) => {
            writeln!(out, "    jmp     {}", label_name(*label)).unwrap();
        }

        // ---- 函数调用与返回 ----
        AsmInst::Call(label) => {
            writeln!(out, "    call    {}", label_name(*label)).unwrap();
        }

        AsmInst::Ret => {
            writeln!(out, "    ret").unwrap();
        }

        // ---- 字符串操作相关 ----
        AsmInst::Cld => {
            writeln!(out, "    cld                         ; 清除方向标志").unwrap();
        }

        AsmInst::RepMovsb => {
            writeln!(
                out,
                "    rep movsb                   ; 复制 rcx 字节: [rsi] -> [rdi]"
            )
            .unwrap();
        }

        // ---- 系统调用 ----
        AsmInst::Syscall => {
            writeln!(out, "    syscall").unwrap();
        }
    }
}

// ============================================================================
// 方案 B：带偏移量的 Hex Dump Listing
// ============================================================================

/// 一条指令在 hex listing 中的记录。
///
/// 包含该指令编码后在 .text 段中的起始偏移、编码后的字节序列、
/// 以及对应的汇编助记符文本。
struct HexListingEntry {
    /// 该指令在 .text 段中的起始偏移（字节）
    offset: usize,

    /// 编码后的机器码字节
    bytes: Vec<u8>,

    /// 汇编助记符文本（与方案 A 中的格式一致，但不含换行和缩进）
    mnemonic: String,

    /// 是否为标签（标签不产生字节，只标记位置）
    is_label: bool,
}

/// 方案 B 使用的 fixup 种类，与 encode.rs 中的定义保持一致。
#[derive(Debug, Clone, Copy)]
enum FixupKind {
    /// 相对 32 位偏移，从下一条指令的 IP 开始计算：
    /// 实际值 = target_offset - (fixup_at + 4)
    Rel32FromNextInsn,
}

/// 方案 B 使用的 fixup 记录。
#[derive(Debug, Clone, Copy)]
struct Fixup {
    /// 目标标签
    label: AsmLabel,

    /// 在 bytes 缓冲区中需要回填的偏移位置
    at: usize,

    /// fixup 的种类
    kind: FixupKind,
}

/// 方案 B 的代码缓冲区。
///
/// 与 encode.rs 中的 `CodeBuffer` 功能相同，但额外记录了
/// 每条指令编码前后的偏移位置，用于生成 hex listing。
///
/// 之所以不直接复用 encode.rs 的 `CodeBuffer`（它是 private 的），
/// 而是在这里重新实现一份，是为了：
/// 1. 不侵入现有编码逻辑
/// 2. 可以在 debug 模块中独立演进
/// 3. 保持 encode.rs 的简洁性（生产路径不含调试开销）
struct DebugCodeBuffer {
    /// 编码后的机器码字节
    bytes: Vec<u8>,

    /// 标签到偏移的映射表
    labels: HashMap<AsmLabel, usize>,

    /// 待回填的 fixup 列表
    fixups: Vec<Fixup>,
}

impl DebugCodeBuffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    /// 返回当前写入位置（即已写入的字节数）
    fn pos(&self) -> usize {
        self.bytes.len()
    }

    /// 将标签绑定到当前偏移位置
    fn bind_label(&mut self, label: AsmLabel) {
        self.labels.insert(label, self.pos());
    }

    /// 写入单个字节
    fn emit_u8(&mut self, b: u8) {
        self.bytes.push(b);
    }

    /// 写入 32 位有符号整数（小端序）
    fn emit_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// 写入 64 位有符号整数（小端序）
    fn emit_i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// 写入一个待回填的 rel32 占位符（4 字节零），并记录 fixup。
    ///
    /// 在 `finish()` 阶段，这 4 个字节会被替换为：
    /// `target_label_offset - (fixup_at + 4)`
    fn emit_rel32_fixup(&mut self, label: AsmLabel) {
        let at = self.pos();
        self.emit_i32(0); // 占位符，后续 finish 时回填
        self.fixups.push(Fixup {
            label,
            at,
            kind: FixupKind::Rel32FromNextInsn,
        });
    }

    /// 完成编码：遍历所有 fixup，计算并回填相对偏移。
    ///
    /// 回填逻辑：
    /// - 找到目标标签的偏移 `target`
    /// - 计算 `rel = target - (fixup.at + 4)`
    ///   （+4 是因为 rel32 相对于它自身之后的下一条指令的 IP）
    /// - 将 rel 截断为 i32 并写入
    fn finish(&mut self) {
        for fixup in &self.fixups.clone() {
            let target = *self
                .labels
                .get(&fixup.label)
                .unwrap_or_else(|| panic!("debug: unknown label {:?}", fixup.label))
                as i64;

            let next_ip = (fixup.at + 4) as i64;
            let rel = target - next_ip;
            let rel32 = i32::try_from(rel).expect("debug: rel32 out of range");

            match fixup.kind {
                FixupKind::Rel32FromNextInsn => {
                    self.bytes[fixup.at..fixup.at + 4].copy_from_slice(&rel32.to_le_bytes());
                }
            }
        }
    }
}

/// 【方案 B】将 `AsmProgram` 编码为机器码，并生成带偏移量的 hex listing。
///
/// 输出格式示例：
/// ```text
/// === Brainfuck x86_64 Hex Listing ===
/// 共 42 条指令，编码后 320 字节
///
/// Offset   Hex                                          Assembly
/// -------- -------------------------------------------- --------------------------------
/// 0x0000:  48 b8 09 00 00 00 00 00 00 00                mov     rax, 0x9 ; sys_mmap
/// 0x000a:  48 bf 00 00 00 00 00 00 00 00                mov     rdi, 0x0
///          <__ensure_tape>:
/// 0x0014:  4d 89 f7                                     mov     r15, r14
/// ```
///
/// # 参数
/// - `program`: 要编码和生成 listing 的汇编程序
///
/// # 返回值
/// 格式化后的 hex listing 字符串
pub fn dump_hex_listing(program: &AsmProgram) -> String {
    // ---- 第一遍：编码所有指令，并记录每条指令的字节范围 ----
    let mut buf = DebugCodeBuffer::new();
    let mut entries: Vec<HexListingEntry> = Vec::new();

    for inst in &program.insts {
        let start = buf.pos();

        // 编码该指令（写入字节到 buf）
        debug_encode_inst(&mut buf, inst);

        let end = buf.pos();

        // 生成该指令的助记符文本（复用方案 A 的格式化逻辑）
        let mut mnemonic_buf = String::new();
        format_inst_asm(&mut mnemonic_buf, inst);
        // 去除首尾空白和换行
        let mnemonic = mnemonic_buf.trim().to_string();

        let is_label = matches!(inst, AsmInst::Label(_));

        entries.push(HexListingEntry {
            offset: start,
            // 此时 bytes 中的 rel32 占位符还是 0，后续 finish 后再提取最终字节
            bytes: Vec::new(), // 先留空，finish 后再填充
            mnemonic,
            is_label,
        });

        // 记录字节范围（start..end），finish 后用来提取最终字节
        // 我们把范围信息暂存在 bytes 的长度中——不，直接存 start 和 end
        // 这里用一个 trick：先记录范围，finish 后再提取
        let entry = entries.last_mut().unwrap();
        entry.offset = start;
        // 暂时用 offset 和 bytes 长度来恢复范围
        entry.bytes = vec![0; end - start]; // 占位，记录长度
    }

    // ---- 完成 fixup 回填 ----
    buf.finish();

    // ---- 提取每条指令的最终字节 ----
    let mut current_offset = 0usize;
    for entry in &mut entries {
        let len = entry.bytes.len();
        if len > 0 {
            entry.bytes = buf.bytes[current_offset..current_offset + len].to_vec();
        }
        current_offset += len;
    }

    let total_bytes = buf.bytes.len();

    // ---- 格式化输出 ----
    let mut out = String::new();

    writeln!(out, "; === Brainfuck x86_64 Hex Listing ===").unwrap();
    writeln!(
        out,
        "; 共 {} 条指令，编码后 {} 字节",
        program.insts.len(),
        total_bytes
    )
    .unwrap();
    writeln!(out).unwrap();

    // 表头
    writeln!(out, "{:<9} {:<44} {}", "Offset", "Hex", "Assembly").unwrap();
    writeln!(
        out,
        "{} {} {}",
        "-".repeat(9),
        "-".repeat(44),
        "-".repeat(40)
    )
    .unwrap();

    for entry in &entries {
        if entry.is_label {
            // 标签不产生字节，特殊格式显示
            writeln!(out, "         {:<44} {}", "", entry.mnemonic).unwrap();
        } else if entry.bytes.is_empty() {
            // 不产生字节的伪指令（理论上不应该出现在非标签情况）
            continue;
        } else {
            // ---- 将字节格式化为十六进制字符串 ----
            // 每行最多显示 14 个字节（占 42 个字符宽度 + 2 字符余量 = 44）
            let hex_per_line = 14;
            let chunks: Vec<&[u8]> = entry.bytes.chunks(hex_per_line).collect();

            for (i, chunk) in chunks.iter().enumerate() {
                let hex_str: String = chunk
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");

                if i == 0 {
                    // 第一行：显示偏移和助记符
                    writeln!(
                        out,
                        "0x{:04x}:  {:<44} {}",
                        entry.offset, hex_str, entry.mnemonic
                    )
                    .unwrap();
                } else {
                    // 续行：只显示剩余的字节（超长指令，如 mov r64, imm64 = 10 字节）
                    writeln!(out, "         {}", hex_str).unwrap();
                }
            }
        }
    }

    // ---- 尾部：输出总字节数摘要 ----
    writeln!(out).unwrap();
    writeln!(out, "; 总计 {} 字节机器码", total_bytes).unwrap();

    out
}

// ============================================================================
// 方案 B 的编码逻辑（完整复制自 encode.rs，仅改用 DebugCodeBuffer）
// ============================================================================
//
// 注意：这里的编码逻辑必须与 encode.rs 中的 `encode_inst` 完全一致，
// 否则 hex listing 中显示的字节就不是实际二进制文件中的字节。
//
// 如果将来修改了 encode.rs 中的编码逻辑，这里也需要同步修改。
// 一个更好的做法是将 encode.rs 中的 CodeBuffer 抽象为 trait，
// 让 DebugCodeBuffer 和生产 CodeBuffer 共享编码实现。
// 但为了保持当前代码的简洁性和独立性，我们先采用复制的方式。

/// 返回寄存器在 x86_64 编码中的数字编号。
///
/// x86_64 使用 3 位编码寄存器：
/// - 低 3 位放入 ModRM 或 opcode 的 reg/rm 字段
/// - 第 4 位（bit 3）放入 REX 前缀的 R/B/X 位
///
/// 通用寄存器编号：
///   rax=0, rcx=1, rdx=2, rbx=3, rsp=4, rbp=5, rsi=6, rdi=7
///   r8=8, r9=9, ..., r15=15
fn reg_num(reg: Reg64) -> u8 {
    match reg {
        Reg64::Rax => 0,
        Reg64::Rcx => 1,
        Reg64::Rdx => 2,
        Reg64::Rsi => 6,
        Reg64::Rdi => 7,
        Reg64::R8 => 8,
        Reg64::R9 => 9,
        Reg64::R10 => 10,
        Reg64::R11 => 11,
        Reg64::R12 => 12,
        Reg64::R13 => 13,
        Reg64::R14 => 14,
        Reg64::R15 => 15,
    }
}

/// 发射 REX.W 前缀字节。
///
/// REX 前缀的格式为：`0100 WRXB`
/// - W=1：操作数宽度为 64 位（REX.W 前缀的核心作用）
/// - R：扩展 ModRM 的 reg 字段（第 4 位）
/// - X：扩展 SIB 的 index 字段（第 4 位）
/// - B：扩展 ModRM 的 rm 字段或 opcode 的 reg 字段（第 4 位）
///
/// `0x48` = `0100_1000`，即 W=1, R=0, X=0, B=0 的基础值。
fn emit_rex_w(buf: &mut DebugCodeBuffer, r: u8, x: u8, b: u8) {
    let rex = 0x48 | ((r & 1) << 2) | ((x & 1) << 1) | (b & 1);
    buf.emit_u8(rex);
}

/// 发射"寄存器 op 立即数32"格式的指令。
///
/// 适用于 `add r64, imm32` / `cmp r64, imm32` 等指令。
///
/// 编码格式：`REX.W + 0x81 + ModRM(11, subcode, rm) + imm32`
///
/// # 参数
/// - `subcode`：ModRM 的 reg 字段（也叫 opcode extension），不同操作使用不同值：
///   - 0 = add
///   - 5 = sub
///   - 7 = cmp
/// - `reg`：目标寄存器
/// - `imm`：32 位符号扩展立即数
fn emit_reg_imm32(buf: &mut DebugCodeBuffer, subcode: u8, reg: Reg64, imm: i32) {
    let rm = reg_num(reg);
    // REX.W 前缀：B 位 = rm 的第 4 位（用于扩展寄存器 R8-R15）
    emit_rex_w(buf, 0, 0, rm >> 3);
    // 操作码 0x81：/r imm32 格式
    buf.emit_u8(0x81);
    // ModRM 字节：mod=11(寄存器直接寻址), reg=subcode, rm=寄存器低3位
    buf.emit_u8(0b11_000_000 | ((subcode & 7) << 3) | (rm & 7));
    // 32 位立即数（符号扩展到 64 位）
    buf.emit_i32(imm);
}

/// 发射逻辑右移指令：`shr reg, imm8`。
///
/// 编码格式：`REX.W + 0xC1 + ModRM(11, 5, rm) + imm8`
///
/// ModRM 的 reg 字段（subcode）= 5 表示 SHR 操作。
fn emit_shift_right_imm8(buf: &mut DebugCodeBuffer, reg: Reg64, imm: u8) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    // 0xC1：移位指令组（带 imm8 操作数）
    buf.emit_u8(0xC1);
    // ModRM：mod=11, reg=5(SHR), rm=寄存器
    buf.emit_u8(0b11_000_000 | (5 << 3) | (rm & 7));
    buf.emit_u8(imm);
}

/// 发射条件跳转指令：`0F cc rel32`。
///
/// x86_64 的近条件跳转使用两字节操作码 `0F xx`，其中 xx 编码条件：
/// - 0x84 = JZ/JE    （ZF=1）
/// - 0x85 = JNZ/JNE  （ZF=0）
/// - 0x82 = JB/JNAE  （CF=1，无符号小于）
/// - 0x83 = JAE/JNB  （CF=0，无符号大于等于）
/// - 0x8C = JL/JNGE  （SF≠OF，有符号小于）
/// - 0x8D = JGE/JNL  （SF=OF，有符号大于等于）
fn emit_jcc_rel32(buf: &mut DebugCodeBuffer, cc: u8, label: AsmLabel) {
    buf.emit_u8(0x0F);
    buf.emit_u8(cc);
    buf.emit_rel32_fixup(label);
}

/// 将单条 `AsmInst` 编码为机器码字节，写入 `DebugCodeBuffer`。
///
/// 此函数的逻辑与 `encode.rs` 中的 `encode_inst` 完全一致。
/// 每种指令的编码方式在注释中详细说明。
fn debug_encode_inst(buf: &mut DebugCodeBuffer, inst: &AsmInst) {
    match inst {
        // ========== 标签（伪指令，不产生字节） ==========
        AsmInst::Label(label) => {
            buf.bind_label(*label);
        }

        // ========== lea reg, [rip + label] ==========
        // RIP 相对寻址的 LEA 指令。
        //
        // 编码：REX + 0x8D + ModRM(00, reg, 101)
        // 其中 ModRM 的 mod=00, rm=101 表示 [RIP + disp32] 寻址模式。
        //
        // 当前仅支持 R13 寄存器（硬编码 REX=0x4C, ModRM=0x2D）。
        // 0x4C = 0100_1100 → W=1, R=1, X=0, B=0（R 位用于扩展 reg 字段到 r13）
        // 0x2D = 00_101_101 → mod=00, reg=101(r13 低3位), rm=101(RIP 相对)
        AsmInst::LeaRipLabel(reg, label) => {
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x4C); // REX.WR
                    buf.emit_u8(0x8D); // LEA 操作码
                    buf.emit_u8(0x2D); // ModRM: [rip + disp32], reg=r13
                    buf.emit_rel32_fixup(*label);
                }
                _ => panic!("LeaRipLabel unsupported register: {:?}", reg),
            }
        }

        // ========== mov r64, imm64 ==========
        // 64 位立即数加载，是 x86_64 中唯一能直接加载 64 位常量的指令。
        //
        // 编码：REX.W + (0xB8 + rd) + imm64
        // 其中 rd 是目标寄存器编号的低 3 位，嵌入操作码本身。
        AsmInst::MovRegImm64(reg, imm) => {
            let code = reg_num(*reg);
            emit_rex_w(buf, 0, 0, code >> 3);
            buf.emit_u8(0xB8 + (code & 7)); // 操作码低 3 位编码目标寄存器
            buf.emit_i64(*imm);
        }

        // ========== mov dst, src （寄存器到寄存器） ==========
        // 编码：REX.W + 0x89 + ModRM(11, src, dst)
        // 注意：0x89 的方向是 src → r/m，所以 reg 字段是 src，rm 字段是 dst。
        AsmInst::MovRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x89);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== add r64, imm32 ==========
        // subcode=0 表示 ADD 操作
        AsmInst::AddRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 0, *reg, *imm);
        }

        // ========== add dst, src ==========
        // 编码：REX.W + 0x01 + ModRM(11, src, dst)
        // 0x01 是 ADD r/m64, r64 的操作码
        AsmInst::AddRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x01);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== sub dst, src ==========
        // 编码：REX.W + 0x29 + ModRM(11, src, dst)
        // 0x29 是 SUB r/m64, r64 的操作码
        AsmInst::SubRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x29);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== cmp lhs, rhs ==========
        // 编码：REX.W + 0x39 + ModRM(11, rhs, lhs)
        // 0x39 是 CMP r/m64, r64 的操作码
        // 计算 lhs - rhs 并设置标志位，但不保存结果
        AsmInst::CmpRegReg(lhs, rhs) => {
            emit_rex_w(buf, reg_num(*rhs) >> 3, 0, reg_num(*lhs) >> 3);
            buf.emit_u8(0x39);
            buf.emit_u8(0b11_000_000 | ((reg_num(*rhs) & 7) << 3) | (reg_num(*lhs) & 7));
        }

        // ========== cmp r64, imm32 ==========
        // subcode=7 表示 CMP 操作
        AsmInst::CmpRegImm32(reg, imm) => emit_reg_imm32(buf, 7, *reg, *imm),

        // ========== shr r64, imm8 ==========
        AsmInst::ShrRegImm8(reg, imm) => emit_shift_right_imm8(buf, *reg, *imm),

        // ========== add byte ptr [reg + 0], imm8 ==========
        // 内存字节加法，用于 BF 的 '+'/'-' 指令。
        //
        // 编码：REX.W + 0x80 + ModRM(01, 0, rm) + disp8(0x00) + imm8
        //
        // ModRM 的 mod=01 表示"寄存器间接 + 8 位偏移"，disp8=0x00。
        // 之所以用 mod=01 + disp8=0 而不是 mod=00（无偏移），
        // 是因为当 rm=101(r13 低3位) 且 mod=00 时，x86_64 会将其
        // 解释为 [RIP + disp32] 而不是 [r13]。这是 x86_64 编码的一个陷阱。
        //
        // ⚠ 注意：当 rm & 7 == 4（即 R12）时，mod=01 需要额外的 SIB 字节，
        //   但当前代码没有发射 SIB。由于 codegen 仅使用 R13，这不会触发。
        AsmInst::AddMem8Imm8(reg, imm) => {
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0x80); // 字节操作 ALU 指令组
            buf.emit_u8(0b01_000_000 | (rm & 7)); // ModRM: mod=01, reg=0(ADD), rm
            buf.emit_u8(0x00); // disp8 = 0
            buf.emit_u8(*imm as u8); // 8 位立即数
        }

        // ========== mov byte ptr [reg + 0], imm8 ==========
        // 内存字节存储，用于 BF 的 [-] 优化（将单元设置为特定值）。
        // 编码与 AddMem8Imm8 类似，操作码改为 0xC6（MOV r/m8, imm8）。
        AsmInst::MovMem8Imm8(reg, imm) => {
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0xC6); // MOV r/m8, imm8
            buf.emit_u8(0b01_000_000 | (rm & 7)); // ModRM: mod=01, reg=0, rm
            buf.emit_u8(0x00); // disp8 = 0
            buf.emit_u8(*imm);
        }

        // ========== cmp byte ptr [reg + 0], imm8 ==========
        // 内存字节比较，用于 BF 的 '[' 和 ']'（检测当前单元是否为零）。
        // 编码与 AddMem8Imm8 类似，但 ModRM 的 reg 字段 = 7（CMP）。
        AsmInst::CmpMem8Imm8(reg, imm) => {
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0x80); // 字节操作 ALU 指令组
            buf.emit_u8(0b01_111_000 | (rm & 7)); // ModRM: mod=01, reg=7(CMP), rm
            buf.emit_u8(0x00); // disp8 = 0
            buf.emit_u8(*imm);
        }

        // ========== 条件跳转（近跳转，32 位相对偏移） ==========
        // 编码格式统一为：0x0F + cc_byte + rel32
        AsmInst::Jz(label) => emit_jcc_rel32(buf, 0x84, *label), // JZ:  ZF=1
        AsmInst::Jnz(label) => emit_jcc_rel32(buf, 0x85, *label), // JNZ: ZF=0
        AsmInst::Jb(label) => emit_jcc_rel32(buf, 0x82, *label), // JB:  CF=1 (无符号<)
        AsmInst::Jae(label) => emit_jcc_rel32(buf, 0x83, *label), // JAE: CF=0 (无符号>=)
        AsmInst::Jl(label) => emit_jcc_rel32(buf, 0x8C, *label), // JL:  SF≠OF (有符号<)
        AsmInst::Jge(label) => emit_jcc_rel32(buf, 0x8D, *label), // JGE: SF=OF (有符号>=)

        // ========== 无条件跳转 ==========
        // 编码：0xE9 + rel32
        AsmInst::Jmp(label) => {
            buf.emit_u8(0xE9);
            buf.emit_rel32_fixup(*label);
        }

        // ========== 函数调用 ==========
        // 编码：0xE8 + rel32
        // CALL 会将返回地址（下一条指令的 RIP）压栈，然后跳转到目标
        AsmInst::Call(label) => {
            buf.emit_u8(0xE8);
            buf.emit_rel32_fixup(*label);
        }

        // ========== 返回 ==========
        // 编码：0xC3
        // 从栈顶弹出返回地址并跳转
        AsmInst::Ret => buf.emit_u8(0xC3),

        // ========== 清除方向标志 ==========
        // 编码：0xFC
        // 确保 REP MOVSB 向前（低地址→高地址）复制
        AsmInst::Cld => buf.emit_u8(0xFC),

        // ========== rep movsb ==========
        // 编码：0xF3 0xA4
        // 重复 rcx 次：将 [rsi] 的一个字节复制到 [rdi]，然后 rsi++, rdi++
        AsmInst::RepMovsb => {
            buf.emit_u8(0xF3); // REP 前缀
            buf.emit_u8(0xA4); // MOVSB 操作码
        }

        // ========== syscall ==========
        // 编码：0x0F 0x05
        // Linux x86_64 系统调用约定：
        //   rax = 系统调用号
        //   rdi, rsi, rdx, r10, r8, r9 = 参数 1~6
        //   返回值在 rax 中
        //   rcx 和 r11 会被内核覆写（clobber）
        AsmInst::Syscall => {
            buf.emit_u8(0x0F);
            buf.emit_u8(0x05);
        }
    }
}

