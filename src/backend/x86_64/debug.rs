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

use std::fmt::Write;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::backend::x86_64::encode::encode_program_with_inst_map;

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

fn format_signed_hex_i32(value: i32) -> String {
    if value < 0 {
        format!("-0x{:x}", value.unsigned_abs())
    } else {
        format!("0x{:x}", value as u32)
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
            writeln!(
                out,
                "    add     {}, {}",
                reg_name(*reg),
                format_signed_hex_i32(*imm)
            )
            .unwrap();
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
            writeln!(
                out,
                "    cmp     {}, {}",
                reg_name(*reg),
                format_signed_hex_i32(*imm)
            )
            .unwrap();
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

        AsmInst::RawBytes(bytes) => {
            writeln!(
                out,
                "    ; <raw {} bytes: precomputed -O3 machine code>",
                bytes.len()
            )
            .unwrap();
        }
    }
}

// ============================================================================
// 方案 B：带偏移量的 Hex Dump Listing
// ============================================================================

/// 【方案 B】将 `AsmProgram` 编码为机器码，并生成带偏移量的 hex listing。
///
/// 机器码字节完全来自生产编码器 `encode.rs`，因此 `.lst` 与真实 ELF `.text`
/// 始终共享同一份编码结果。
pub fn dump_hex_listing(program: &AsmProgram) -> String {
    let (encoded, inst_map) = encode_program_with_inst_map(program);
    let mut out = String::new();

    writeln!(out, "; === Brainfuck x86_64 Hex Listing ===").unwrap();
    writeln!(
        out,
        "; 共 {} 条指令，编码后 {} 字节",
        program.insts.len(),
        encoded.text.len()
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "{:<9} {:<44} {}", "Offset", "Hex", "Assembly").unwrap();
    writeln!(
        out,
        "{} {} {}",
        "-".repeat(9),
        "-".repeat(44),
        "-".repeat(40)
    )
    .unwrap();

    for (inst, encoded_inst) in program.insts.iter().zip(inst_map.iter()) {
        let mut mnemonic_buf = String::new();
        format_inst_asm(&mut mnemonic_buf, inst);
        let mnemonic = mnemonic_buf.trim();

        if matches!(inst, AsmInst::Label(_)) {
            writeln!(out, "         {:<44} {}", "", mnemonic).unwrap();
            continue;
        }

        if encoded_inst.len == 0 {
            continue;
        }

        let bytes = &encoded.text[encoded_inst.offset..encoded_inst.offset + encoded_inst.len];
        for (line_idx, chunk) in bytes.chunks(14).enumerate() {
            let hex_str = chunk
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");

            if line_idx == 0 {
                writeln!(
                    out,
                    "0x{:04x}:  {:<44} {}",
                    encoded_inst.offset, hex_str, mnemonic
                )
                .unwrap();
            } else {
                writeln!(out, "         {}", hex_str).unwrap();
            }
        }
    }

    writeln!(out).unwrap();
    writeln!(out, "; 总计 {} 字节机器码", encoded.text.len()).unwrap();

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x86_64::encode::encode_program;

    fn parse_hex_listing_bytes(listing: &str) -> Vec<u8> {
        let mut bytes = Vec::new();

        for line in listing.lines() {
            if let Some(rest) = line.strip_prefix("0x") {
                let Some((_, after_colon)) = rest.split_once(':') else {
                    continue;
                };
                let Some((hex_field, _)) = after_colon.trim_start().split_once("  ") else {
                    continue;
                };
                extend_hex_bytes(&mut bytes, hex_field);
                continue;
            }

            if let Some(rest) = line.strip_prefix("         ") {
                let trimmed = rest.trim();
                if !trimmed.is_empty() && trimmed.bytes().all(is_hex_listing_char) {
                    extend_hex_bytes(&mut bytes, trimmed);
                }
            }
        }

        bytes
    }

    fn extend_hex_bytes(out: &mut Vec<u8>, field: &str) {
        for byte in field.split_whitespace() {
            out.push(u8::from_str_radix(byte, 16).unwrap());
        }
    }

    fn is_hex_listing_char(byte: u8) -> bool {
        byte.is_ascii_hexdigit() || byte == b' '
    }

    #[test]
    fn hex_listing_bytes_match_production_encoder_output() {
        let program = AsmProgram {
            insts: vec![
                AsmInst::MovRegImm64(Reg64::Rax, 9),
                AsmInst::MovMem8Imm8(Reg64::R13, 0),
                AsmInst::Label(AsmLabel(7)),
                AsmInst::CmpMem8Imm8(Reg64::R13, 0),
                AsmInst::Jz(AsmLabel(7)),
                AsmInst::Ret,
            ],
        };

        let encoded = encode_program(&program);
        let listing = dump_hex_listing(&program);

        assert_eq!(parse_hex_listing_bytes(&listing), encoded.text);
    }
}
