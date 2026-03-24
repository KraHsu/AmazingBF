use std::collections::HashMap;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};

#[derive(Debug, Clone)]
pub struct EncodedProgram {
    /// machine code output
    pub text: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum FixupKind {
    /// reletive 32 bit from next instruction
    /// target - netx_instruction_ip
    Rel32FromNextInsn,
}

/// record of fixup
#[derive(Debug, Clone, Copy)]
struct Fixup {
    /// target label
    label: AsmLabel,

    /// at the offset within `bytes` where to reserve 0x00*4
    at: usize,

    kind: FixupKind,
}

/// code buffer for machine code
///
/// 1. write machine code into `bytes` in order
/// 2. record which label is bind to the offset finally
/// 3. record fixups
struct CodeBuffer {
    bytes: Vec<u8>,

    /// label -> offset
    labels: HashMap<AsmLabel, usize>,

    fixups: Vec<Fixup>,
}

impl CodeBuffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    fn pos(&self) -> usize {
        self.bytes.len()
    }

    fn bind_label(&mut self, label: AsmLabel) {
        self.labels.insert(label, self.pos());
    }

    fn emit_u8(&mut self, b: u8) {
        self.bytes.push(b);
    }

    /// little-endian
    fn emit_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// little-endian
    fn emit_i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn emit_rel32_fixup(&mut self, label: AsmLabel) {
        let at = self.pos();
        self.emit_i32(0);
        self.fixups.push(Fixup {
            label,
            at,
            kind: FixupKind::Rel32FromNextInsn,
        });
    }

    /// finish the coding and output machine code bytes
    ///
    /// - go through all fixups
    /// - find offset of target label
    /// - calculate relative offset
    /// - check if it can be written in i32
    /// - write the offset back
    fn finish(mut self) -> Vec<u8> {
        for fixup in &self.fixups {
            let target = *self
                .labels
                .get(&fixup.label)
                .unwrap_or_else(|| panic!("unknown label: {:?}", fixup.label))
                as i64;

            let next_ip = (fixup.at + 4) as i64;
            let rel = target - next_ip;
            let rel32 = i32::try_from(rel).expect("rel32 out of range");

            match fixup.kind {
                FixupKind::Rel32FromNextInsn => {
                    self.bytes[fixup.at..fixup.at + 4].copy_from_slice(&rel32.to_le_bytes());
                }
            }
        }

        self.bytes
    }
}

pub fn encode_program(program: &AsmProgram) -> EncodedProgram {
    let mut buf = CodeBuffer::new();

    for inst in &program.insts {
        encode_inst(&mut buf, inst);
    }

    EncodedProgram { text: buf.finish() }
}

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

fn emit_rex_w(buf: &mut CodeBuffer, r: u8, x: u8, b: u8) {
    let rex = 0x48 | ((r & 1) << 2) | ((x & 1) << 1) | (b & 1);
    buf.emit_u8(rex);
}

fn emit_modrm_reg_reg(buf: &mut CodeBuffer, reg_field: Reg64, rm_field: Reg64) {
    let reg = reg_num(reg_field);
    let rm = reg_num(rm_field);
    emit_rex_w(buf, reg >> 3, 0, rm >> 3);
    buf.emit_u8(0b11_000_000 | ((reg & 7) << 3) | (rm & 7));
}

fn emit_reg_imm32(buf: &mut CodeBuffer, subcode: u8, reg: Reg64, imm: i32) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(0x81);
    buf.emit_u8(0b11_000_000 | ((subcode & 7) << 3) | (rm & 7));
    buf.emit_i32(imm);
}

fn emit_shift_right_imm8(buf: &mut CodeBuffer, reg: Reg64, imm: u8) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(0xC1);
    buf.emit_u8(0b11_000_000 | (5 << 3) | (rm & 7));
    buf.emit_u8(imm);
}

fn emit_jcc_rel32(buf: &mut CodeBuffer, cc: u8, label: AsmLabel) {
    buf.emit_u8(0x0F);
    buf.emit_u8(cc);
    buf.emit_rel32_fixup(label);
}

fn encode_inst(buf: &mut CodeBuffer, inst: &AsmInst) {
    match inst {
        AsmInst::Label(label) => {
            buf.bind_label(*label);
        }

        AsmInst::LeaRipLabel(reg, label) => {
            // lea reg, [rip + rel32]
            //
            // only support:
            //   lea r13, [rip + label]
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x4C); // REX
                    buf.emit_u8(0x8D); // LEA opcode
                    buf.emit_u8(0x2D); // ModRM
                    buf.emit_rel32_fixup(*label);
                }
                _ => panic!("LeaRipLabel unsupported register: {:?}", reg),
            }
        }

        AsmInst::MovRegImm64(reg, imm) => {
            // mov r64, imm64
            let code = reg_num(*reg);
            emit_rex_w(buf, 0, 0, code >> 3);
            buf.emit_u8(0xB8 + (code & 7));
            buf.emit_i64(*imm);
        }

        AsmInst::MovRegReg(dst, src) => {
            // mov dst, src
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x89);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        AsmInst::AddRegImm32(reg, imm) => {
            // add r64, imm32
            emit_reg_imm32(buf, 0, *reg, *imm);
        }

        AsmInst::AddRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x01);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        AsmInst::SubRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x29);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        AsmInst::CmpRegReg(lhs, rhs) => {
            emit_rex_w(buf, reg_num(*rhs) >> 3, 0, reg_num(*lhs) >> 3);
            buf.emit_u8(0x39);
            buf.emit_u8(0b11_000_000 | ((reg_num(*rhs) & 7) << 3) | (reg_num(*lhs) & 7));
        }

        AsmInst::CmpRegImm32(reg, imm) => emit_reg_imm32(buf, 7, *reg, *imm),

        AsmInst::ShrRegImm8(reg, imm) => emit_shift_right_imm8(buf, *reg, *imm),

        AsmInst::AddMem8Imm8(reg, imm) => {
            // add byte ptr [r/m64], imm8
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0x80);
            buf.emit_u8(0b01_000_000 | (rm & 7));
            buf.emit_u8(0x00);
            buf.emit_u8(*imm as u8);
        }

        AsmInst::MovMem8Imm8(reg, imm) => {
            // mov byte ptr [r/m64], imm8
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0xC6);
            buf.emit_u8(0b01_000_000 | (rm & 7));
            buf.emit_u8(0x00);
            buf.emit_u8(*imm);
        }

        AsmInst::CmpMem8Imm8(reg, imm) => {
            // cmp byte ptr [r/m64], imm8
            let rm = reg_num(*reg);
            emit_rex_w(buf, 0, 0, rm >> 3);
            buf.emit_u8(0x80);
            buf.emit_u8(0b01_111_000 | (rm & 7));
            buf.emit_u8(0x00);
            buf.emit_u8(*imm);
        }

        // 0F 84 rel32
        AsmInst::Jz(label) => emit_jcc_rel32(buf, 0x84, *label),
        // 0F 85 rel32
        AsmInst::Jnz(label) => emit_jcc_rel32(buf, 0x85, *label),
        AsmInst::Jb(label) => emit_jcc_rel32(buf, 0x82, *label),
        AsmInst::Jae(label) => emit_jcc_rel32(buf, 0x83, *label),
        AsmInst::Jl(label) => emit_jcc_rel32(buf, 0x8C, *label),
        AsmInst::Jge(label) => emit_jcc_rel32(buf, 0x8D, *label),

        AsmInst::Jmp(label) => {
            buf.emit_u8(0xE9);
            buf.emit_rel32_fixup(*label);
        }

        AsmInst::Call(label) => {
            buf.emit_u8(0xE8);
            buf.emit_rel32_fixup(*label);
        }

        AsmInst::Ret => buf.emit_u8(0xC3),

        AsmInst::Cld => buf.emit_u8(0xFC),

        AsmInst::RepMovsb => {
            buf.emit_u8(0xF3);
            buf.emit_u8(0xA4);
        }

        AsmInst::Syscall => {
            // just call, but don't check rax
            buf.emit_u8(0x0F);
            buf.emit_u8(0x05);
        }
    }
}
