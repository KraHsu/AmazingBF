//! # x86_64 机器码编码器 (encode.rs)
//!
//! 本模块负责将 `AsmProgram`（汇编 IR）编码为实际的 x86_64 机器码字节序列。
//!
//! ## 编码流程
//!
//! 1. **顺序编码**：遍历每条 `AsmInst`，按 x86_64 指令格式写入字节。
//!    跳转/调用指令的目标偏移暂时填入 0，并记录一个 fixup。
//!
//! 2. **标签绑定**：遇到 `Label` 伪指令时，记录该标签对应的字节偏移。
//!
//! 3. **Fixup 回填**：所有指令编码完成后，遍历 fixup 列表：
//!    - 查找目标标签的偏移
//!    - 计算相对偏移 `rel = target - next_ip`
//!    - 将 rel32 写回之前预留的 4 字节占位符
//!
//! ## x86_64 指令编码基础
//!
//! 一条典型的 x86_64 指令由以下部分组成（非所有部分都必须存在）：
//!
//! ```text
//! [前缀] [REX] [操作码(1-3字节)] [ModRM] [SIB] [偏移量] [立即数]
//! ```
//!
//! ### REX 前缀 (0x40 ~ 0x4F)
//!
//! 格式：`0100 WRXB`
//! - W：操作数宽度为 64 位
//! - R：扩展 ModRM.reg（从 3 位扩展到 4 位，支持 R8-R15）
//! - X：扩展 SIB.index
//! - B：扩展 ModRM.rm 或操作码的寄存器字段
//!
//! ### ModRM 字节
//!
//! 格式：`mm_rrr_mmm`（2+3+3 位）
//! - mod (mm)：寻址模式
//!   - 00 = 寄存器间接（[reg]，特殊情况：rm=101 → [RIP+disp32]）
//!   - 01 = 寄存器间接 + 8 位偏移（[reg+disp8]）
//!   - 10 = 寄存器间接 + 32 位偏移（[reg+disp32]）
//!   - 11 = 寄存器直接（reg 本身）
//! - reg (rrr)：寄存器操作数或操作码扩展（subcode）
//! - rm (mmm)：寄存器/内存操作数（特殊情况：rm=100 → 后接 SIB 字节）

use std::collections::HashMap;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};

/// 编码后的程序，包含最终的机器码字节序列。
#[derive(Debug, Clone)]
pub struct EncodedProgram {
    /// .text 段的机器码字节，可直接写入 ELF 文件
    pub text: Vec<u8>,
}

/// 单条 `AsmInst` 在最终 `.text` 中对应的字节范围。
///
/// `dump_hex_listing()` 使用这份元数据直接从生产编码结果切片，
/// 避免再维护一套独立的调试编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EncodedInst {
    /// 指令在 `.text` 中的起始偏移。
    pub offset: usize,
    /// 指令编码后的字节长度。标签等伪指令长度为 0。
    pub len: usize,
}

/// Fixup 的种类。
///
/// 目前仅支持一种：从下一条指令的 IP 计算的 32 位相对偏移。
/// 这覆盖了 x86_64 中所有近跳转（JMP/Jcc）和近调用（CALL）的需求。
#[derive(Debug, Clone, Copy)]
enum FixupKind {
    /// 32 位相对偏移，基准点为 fixup 位置之后 4 字节处（即下一条指令的 IP）。
    ///
    /// 计算公式：`rel32 = target_offset - (fixup_at + 4)`
    ///
    /// 之所以 +4，是因为 CPU 在执行跳转时，IP 已经指向了 rel32 字段之后。
    Rel32FromNextInsn,
}

/// 一个待回填的记录。
///
/// 在编码阶段，当遇到跳转/调用指令时，目标偏移还不确定
/// （可能是前向引用），所以先写入 4 字节零占位符，
/// 并记录一个 Fixup，留待所有指令编码完成后再回填。
#[derive(Debug, Clone, Copy)]
struct Fixup {
    /// 跳转/调用的目标标签
    label: AsmLabel,

    /// 占位符在 `bytes` 缓冲区中的起始偏移
    at: usize,

    /// fixup 的种类（决定如何计算回填值）
    kind: FixupKind,
}

/// 机器码编码缓冲区。
///
/// 提供了一组底层方法用于：
/// - 写入字节/整数到缓冲区
/// - 绑定标签到当前偏移
/// - 记录 fixup
/// - 最终回填所有 fixup 并输出完整的机器码
///
/// 使用流程：
/// ```text
/// 1. 创建 CodeBuffer
/// 2. 对每条指令：调用 encode_inst 写入字节
/// 3. 调用 finish() 回填 fixup 并取出 bytes
/// ```
struct CodeBuffer {
    /// 编码后的机器码字节缓冲区
    bytes: Vec<u8>,

    /// 标签到字节偏移的映射。
    ///
    /// 在编码过程中，每遇到一个 Label 伪指令，
    /// 就将该标签映射到当前的 `bytes.len()`。
    labels: HashMap<AsmLabel, usize>,

    /// 待回填的 fixup 列表。
    ///
    /// 在 finish() 阶段统一处理。
    fixups: Vec<Fixup>,
}

impl CodeBuffer {
    /// 创建一个空的代码缓冲区
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    /// 返回当前写入位置（即已写入的字节数）。
    ///
    /// 这个值同时也是下一个字节将要写入的偏移位置。
    fn pos(&self) -> usize {
        self.bytes.len()
    }

    /// 将标签绑定到当前偏移位置。
    ///
    /// 之后的 fixup 回填会使用这个偏移作为跳转目标。
    /// 如果同一个标签被绑定两次，后者会覆盖前者（通常这是 bug）。
    fn bind_label(&mut self, label: AsmLabel) {
        let previous = self.labels.insert(label, self.pos());
        assert!(
            previous.is_none(),
            "label bound multiple times: {:?}",
            label
        );
    }

    /// 写入单个字节
    fn emit_u8(&mut self, b: u8) {
        self.bytes.push(b);
    }

    fn emit_bytes(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
    }

    /// 写入 32 位有符号整数（小端序）。
    ///
    /// x86_64 使用小端序，即最低有效字节在最低地址。
    fn emit_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// 写入 64 位有符号整数（小端序）。
    fn emit_i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// 写入一个 rel32 占位符（4 字节零），并记录对应的 fixup。
    ///
    /// 这是跳转和调用指令的核心：先预留空间，事后回填。
    fn emit_rel32_fixup(&mut self, label: AsmLabel) {
        let at = self.pos();
        self.emit_i32(0); // 4 字节零占位符
        self.fixups.push(Fixup {
            label,
            at,
            kind: FixupKind::Rel32FromNextInsn,
        });
    }

    /// 完成编码：回填所有 fixup，返回最终的机器码字节序列。
    ///
    /// ## Fixup 回填算法
    ///
    /// 对于每个 `Rel32FromNextInsn` 类型的 fixup：
    /// 1. 从 `labels` 中查找目标标签的偏移 `target`
    /// 2. 计算下一条指令的 IP：`next_ip = fixup.at + 4`
    /// 3. 计算相对偏移：`rel = target - next_ip`
    /// 4. 检查 `rel` 是否在 i32 范围内（±2GB）
    /// 5. 将 `rel` 的小端序表示写回 `bytes[fixup.at..fixup.at+4]`
    ///
    /// ## Panic
    ///
    /// - 如果引用了未绑定的标签
    /// - 如果相对偏移超出 i32 范围（代码段 > 2GB 时才会发生）
    fn finish(mut self) -> Vec<u8> {
        for fixup in &self.fixups {
            // 查找目标标签的偏移
            let target = *self
                .labels
                .get(&fixup.label)
                .unwrap_or_else(|| panic!("unknown label: {:?}", fixup.label))
                as i64;

            // 下一条指令的 IP = fixup 位置 + 4 字节（rel32 字段本身的长度）
            let next_ip = (fixup.at + 4) as i64;

            // 相对偏移 = 目标 - 下一条指令的 IP
            let rel = target - next_ip;

            // 安全检查：确保相对偏移能用 32 位表示
            let rel32 = i32::try_from(rel).expect("rel32 out of range");

            match fixup.kind {
                FixupKind::Rel32FromNextInsn => {
                    // 将计算出的相对偏移写回占位符位置
                    self.bytes[fixup.at..fixup.at + 4].copy_from_slice(&rel32.to_le_bytes());
                }
            }
        }

        self.bytes
    }
}

/// 将 `AsmProgram` 编码为机器码。
///
/// 这是本模块的入口函数，供 `x86_64/mod.rs` 调用。
pub fn encode_program(program: &AsmProgram) -> EncodedProgram {
    encode_program_with_inst_map(program).0
}

/// 将 `AsmProgram` 编码为机器码，并保留每条指令在 `.text` 中的范围信息。
pub(crate) fn encode_program_with_inst_map(
    program: &AsmProgram,
) -> (EncodedProgram, Vec<EncodedInst>) {
    let mut buf = CodeBuffer::new();
    let mut inst_map = Vec::with_capacity(program.insts.len());

    for inst in &program.insts {
        let offset = buf.pos();
        encode_inst(&mut buf, inst);
        inst_map.push(EncodedInst {
            offset,
            len: buf.pos() - offset,
        });
    }

    (
        EncodedProgram { text: buf.finish() },
        inst_map,
    )
}

// ============================================================================
// 辅助编码函数
// ============================================================================

/// 返回寄存器在 x86_64 编码中的数字编号。
///
/// 低 3 位用于 ModRM/SIB/opcode，第 4 位用于 REX 的 R/B/X 扩展。
///
/// 注意：编号中跳过了 3(RBX)、4(RSP)、5(RBP)，因为本编译器不使用这些寄存器。
fn reg_num(reg: Reg64) -> u8 {
    match reg {
        Reg64::Rax => 0,  // 000
        Reg64::Rcx => 1,  // 001
        Reg64::Rdx => 2,  // 010
        Reg64::Rsi => 6,  // 110
        Reg64::Rdi => 7,  // 111
        Reg64::R8 => 8,   // 1_000
        Reg64::R9 => 9,   // 1_001
        Reg64::R10 => 10, // 1_010
        Reg64::R11 => 11, // 1_011
        Reg64::R12 => 12, // 1_100
        Reg64::R13 => 13, // 1_101
        Reg64::R14 => 14, // 1_110
        Reg64::R15 => 15, // 1_111
    }
}

/// 发射 REX.W 前缀字节。
///
/// REX 字节格式：`0100 WRXB`
///
/// 参数 `r`, `x`, `b` 分别是 R/X/B 位的值（仅低 1 位有效）：
/// - `r`：扩展 ModRM.reg 字段 → 传入 `reg_num >> 3`
/// - `x`：扩展 SIB.index 字段 → 通常为 0（本编译器不使用 SIB）
/// - `b`：扩展 ModRM.rm 字段 → 传入 `rm_num >> 3`
///
/// 基础值 0x48 = `0100_1000`（W=1, R=0, X=0, B=0），
/// 表示 64 位操作数宽度。
fn emit_rex_w(buf: &mut CodeBuffer, r: u8, x: u8, b: u8) {
    let rex = 0x48 | ((r & 1) << 2) | ((x & 1) << 1) | (b & 1);
    buf.emit_u8(rex);
}

/// 发射 "寄存器 op 符号扩展的32位立即数" 格式的指令。
///
/// 编码：`REX.W + 0x81 + ModRM(11, subcode, rm) + imm32`
///
/// `subcode` 是 ModRM.reg 字段，用作操作码扩展：
/// - 0 → ADD
/// - 1 → OR
/// - 4 → AND
/// - 5 → SUB
/// - 7 → CMP
///
/// imm32 会被 CPU 符号扩展到 64 位。
fn emit_reg_imm32(buf: &mut CodeBuffer, subcode: u8, reg: Reg64, imm: i32) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(0x81); // 操作码：r/m64, imm32 格式
    buf.emit_u8(0b11_000_000 | ((subcode & 7) << 3) | (rm & 7));
    buf.emit_i32(imm);
}

/// 发射逻辑右移指令：`shr reg, imm8`。
///
/// 编码：`REX.W + 0xC1 + ModRM(11, 5, rm) + imm8`
///
/// ModRM.reg = 5 表示 SHR 操作（同一操作码组中：4=SHL, 5=SHR, 7=SAR）。
fn emit_shift_right_imm8(buf: &mut CodeBuffer, reg: Reg64, imm: u8) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(0xC1); // 移位指令组（imm8 操作数）
    buf.emit_u8(0b11_000_000 | (5 << 3) | (rm & 7)); // subcode=5 → SHR
    buf.emit_u8(imm);
}

fn emit_mem8_disp0(buf: &mut CodeBuffer, opcode: u8, subcode: u8, reg: Reg64, imm: u8) {
    let rm = reg_num(reg);
    assert!(
        (rm & 7) != 4,
        "mem8 disp0 encoding requires SIB for register {:?}",
        reg
    );
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(opcode);
    buf.emit_u8(0b01_000_000 | ((subcode & 7) << 3) | (rm & 7));
    buf.emit_u8(0x00);
    buf.emit_u8(imm);
}

/// 发射条件跳转指令：`0F cc rel32`。
///
/// 所有近条件跳转共享这个编码格式，cc 字节决定跳转条件。
/// rel32 是相对于下一条指令 IP 的偏移。
fn emit_jcc_rel32(buf: &mut CodeBuffer, cc: u8, label: AsmLabel) {
    buf.emit_u8(0x0F); // 双字节操作码前缀
    buf.emit_u8(cc); // 条件码
    buf.emit_rel32_fixup(label); // 32 位相对偏移（待回填）
}

// ============================================================================
// 主编码函数
// ============================================================================

/// 将单条 `AsmInst` 编码为机器码字节，写入 `CodeBuffer`。
///
/// 每种指令的编码细节在各 match 分支中详细注释。
fn encode_inst(buf: &mut CodeBuffer, inst: &AsmInst) {
    match inst {
        // ========== 伪指令 ==========
        AsmInst::Label(label) => {
            // 标签不产生字节，只记录当前偏移
            buf.bind_label(*label);
        }

        // ========== mov r64, imm64 ==========
        //
        // 唯一的 64 位立即数加载指令。
        // 编码：REX.W + (0xB8 + rd) + imm64
        //
        // 操作码 0xB8~0xBF 的低 3 位直接编码目标寄存器编号。
        // 总长度：10 字节（1 REX + 1 opcode + 8 imm64）。
        AsmInst::MovRegImm64(reg, imm) => {
            let code = reg_num(*reg);
            emit_rex_w(buf, 0, 0, code >> 3);
            buf.emit_u8(0xB8 + (code & 7));
            buf.emit_i64(*imm);
        }

        // ========== mov dst, src ==========
        //
        // 编码：REX.W + 0x89 + ModRM(11, src, dst)
        //
        // 0x89 的方向是 r → r/m，所以：
        // - ModRM.reg = src（源寄存器）
        // - ModRM.rm  = dst（目标寄存器）
        AsmInst::MovRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x89);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== add r64, imm32 ==========
        AsmInst::AddRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 0, *reg, *imm); // subcode 0 = ADD
        }

        // ========== add dst, src ==========
        //
        // 编码：REX.W + 0x01 + ModRM(11, src, dst)
        // 0x01 是 ADD r/m64, r64 的操作码
        AsmInst::AddRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x01);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== sub dst, src ==========
        //
        // 编码：REX.W + 0x29 + ModRM(11, src, dst)
        // 0x29 是 SUB r/m64, r64 的操作码
        AsmInst::SubRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x29);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // ========== cmp lhs, rhs ==========
        //
        // 编码：REX.W + 0x39 + ModRM(11, rhs, lhs)
        // 0x39 是 CMP r/m64, r64 的操作码
        // 语义：计算 lhs - rhs，设置 EFLAGS，结果被丢弃
        AsmInst::CmpRegReg(lhs, rhs) => {
            emit_rex_w(buf, reg_num(*rhs) >> 3, 0, reg_num(*lhs) >> 3);
            buf.emit_u8(0x39);
            buf.emit_u8(0b11_000_000 | ((reg_num(*rhs) & 7) << 3) | (reg_num(*lhs) & 7));
        }

        // ========== cmp r64, imm32 ==========
        AsmInst::CmpRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 7, *reg, *imm); // subcode 7 = CMP
        }

        // ========== shr r64, imm8 ==========
        AsmInst::ShrRegImm8(reg, imm) => emit_shift_right_imm8(buf, *reg, *imm),

        // ========== add byte ptr [reg+0], imm8 ==========
        //
        // 编码：REX.W + 0x80 + ModRM(01, 0, rm) + disp8(0) + imm8
        //
        // 0x80 是字节 ALU 操作指令组，ModRM.reg=0 表示 ADD。
        //
        // 使用 mod=01（[reg+disp8]）而非 mod=00（[reg]）的原因：
        // 当 rm & 7 == 5（即 R13 的低 3 位）时，mod=00 会被 CPU
        // 解释为 [RIP+disp32] 而不是 [r13]。使用 mod=01 + disp8=0
        // 可以避免这个歧义。
        //
        // ⚠ 潜在问题：当 rm & 7 == 4（即 R12）时，CPU 会期望
        //   ModRM 之后跟一个 SIB 字节，但当前代码没有发射 SIB。
        //   目前安全，因为 codegen 仅对 R13 使用此指令。
        AsmInst::AddMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0x80, 0, *reg, *imm as u8);
        }

        // ========== mov byte ptr [reg+0], imm8 ==========
        //
        // 编码：REX.W + 0xC6 + ModRM(01, 0, rm) + disp8(0) + imm8
        // 0xC6 是 MOV r/m8, imm8 的操作码
        AsmInst::MovMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0xC6, 0, *reg, *imm);
        }

        // ========== cmp byte ptr [reg+0], imm8 ==========
        //
        // 编码：REX.W + 0x80 + ModRM(01, 7, rm) + disp8(0) + imm8
        // ModRM.reg = 7 表示 CMP
        AsmInst::CmpMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0x80, 7, *reg, *imm);
        }

        // ========== 条件跳转 ==========
        // 编码格式统一：0x0F + cc + rel32
        AsmInst::Jz(label) => emit_jcc_rel32(buf, 0x84, *label), // ZF=1
        AsmInst::Jnz(label) => emit_jcc_rel32(buf, 0x85, *label), // ZF=0
        AsmInst::Jb(label) => emit_jcc_rel32(buf, 0x82, *label), // CF=1 (unsigned <)
        AsmInst::Jae(label) => emit_jcc_rel32(buf, 0x83, *label), // CF=0 (unsigned >=)
        AsmInst::Jl(label) => emit_jcc_rel32(buf, 0x8C, *label), // SF≠OF (signed <)
        AsmInst::Jge(label) => emit_jcc_rel32(buf, 0x8D, *label), // SF=OF (signed >=)

        // ========== 无条件跳转 ==========
        // 编码：0xE9 + rel32
        AsmInst::Jmp(label) => {
            buf.emit_u8(0xE9);
            buf.emit_rel32_fixup(*label);
        }

        // ========== 调用 ==========
        // 编码：0xE8 + rel32
        // CALL 隐式执行 push(RIP)，然后 jmp target
        AsmInst::Call(label) => {
            buf.emit_u8(0xE8);
            buf.emit_rel32_fixup(*label);
        }

        // ========== 返回 ==========
        // 编码：0xC3（近返回）
        // RET 隐式执行 pop(RIP)
        AsmInst::Ret => buf.emit_u8(0xC3),

        // ========== 清除方向标志 ==========
        // 编码：0xFC
        AsmInst::Cld => buf.emit_u8(0xFC),

        // ========== rep movsb ==========
        // 编码：0xF3（REP 前缀）+ 0xA4（MOVSB）
        // 重复 rcx 次：byte [rdi++] = byte [rsi++]
        AsmInst::RepMovsb => {
            buf.emit_u8(0xF3);
            buf.emit_u8(0xA4);
        }

        // ========== syscall ==========
        // 编码：0x0F 0x05
        AsmInst::Syscall => {
            buf.emit_u8(0x0F);
            buf.emit_u8(0x05);
        }

        AsmInst::RawBytes(bytes) => {
            buf.emit_bytes(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inst_map_tracks_per_instruction_byte_ranges() {
        let program = AsmProgram {
            insts: vec![
                AsmInst::MovRegImm64(Reg64::Rax, 9),
                AsmInst::Label(AsmLabel(3)),
                AsmInst::Ret,
            ],
        };

        let (encoded, inst_map) = encode_program_with_inst_map(&program);

        assert_eq!(inst_map.len(), program.insts.len());
        assert_eq!(inst_map[0].offset, 0);
        assert_eq!(inst_map[0].len, 10);
        assert_eq!(inst_map[1].offset, 10);
        assert_eq!(inst_map[1].len, 0);
        assert_eq!(inst_map[2].offset, 10);
        assert_eq!(inst_map[2].len, 1);
        assert_eq!(encoded.text.len(), 11);
    }

    #[test]
    #[should_panic(expected = "label bound multiple times")]
    fn duplicate_labels_panic_during_encoding() {
        let program = AsmProgram {
            insts: vec![
                AsmInst::Label(AsmLabel(1)),
                AsmInst::Label(AsmLabel(1)),
                AsmInst::Ret,
            ],
        };

        let _ = encode_program(&program);
    }

    #[test]
    #[should_panic(expected = "mem8 disp0 encoding requires SIB")]
    fn mem8_disp0_rejects_r12_without_sib_support() {
        let program = AsmProgram {
            insts: vec![AsmInst::MovMem8Imm8(Reg64::R12, 1)],
        };

        let _ = encode_program(&program);
    }
}
