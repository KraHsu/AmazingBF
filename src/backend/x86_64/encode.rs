use std::collections::HashMap;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};

#[derive(Debug, Clone)]
pub struct EncodedProgram {
    /// machine code output
    pub text: Vec<u8>,

    /// label for tape
    pub tape_label: AsmLabel,

    /// tape size in byte
    pub tape_size: usize,
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

    EncodedProgram {
        text: buf.finish(),
        tape_label: program.tape_label,
        tape_size: program.tape_size,
    }
}

/// from asm to machine code
/// code xian ren ???
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
            match reg {
                Reg64::Rax => {
                    buf.emit_u8(0x48);
                    buf.emit_u8(0xB8);
                }
                Reg64::Rdi => {
                    buf.emit_u8(0x48);
                    buf.emit_u8(0xBF);
                }
                Reg64::Rsi => {
                    buf.emit_u8(0x48);
                    buf.emit_u8(0xBE);
                }
                Reg64::Rdx => {
                    buf.emit_u8(0x48);
                    buf.emit_u8(0xBA);
                }
                Reg64::R13 => {
                    buf.emit_u8(0x49);
                    buf.emit_u8(0xBD);
                }
            }
            buf.emit_i64(*imm);
        }

        AsmInst::MovRegReg(dst, src) => {
            // mov dst, src
            //
            // only support:
            //   mov rsi, r13
            match (*dst, *src) {
                (Reg64::Rsi, Reg64::R13) => {
                    buf.emit_u8(0x4C);
                    buf.emit_u8(0x89);
                    buf.emit_u8(0xEE);
                }
                _ => panic!("MovRegReg unsupported pair: {:?} <- {:?}", dst, src),
            }
        }

        AsmInst::AddRegImm32(reg, imm) => {
            // add r64, imm32
            //
            // only support:
            //   add r13, imm32
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x49);
                    buf.emit_u8(0x81);
                    buf.emit_u8(0xC5);
                    buf.emit_i32(*imm);
                }
                _ => panic!("AddRegImm32 unsupported register: {:?}", reg),
            }
        }

        AsmInst::AddMem8Imm8(reg, imm) => {
            // add byte ptr [r/m64], imm8
            // 
            // only support:
            //   add byte ptr [r13 + 0], imm8
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x41);
                    buf.emit_u8(0x80);
                    buf.emit_u8(0x45);
                    buf.emit_u8(0x00);
                    buf.emit_u8(*imm as u8);
                }
                _ => panic!("AddMem8Imm8 unsupported register: {:?}", reg),
            }
        }

        AsmInst::MovMem8Imm8(reg, imm) => {
            // mov byte ptr [r/m64], imm8
            //
            // only support:
            //   mov byte ptr [r13 + 0], imm8
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x41);
                    buf.emit_u8(0xC6);
                    buf.emit_u8(0x45);
                    buf.emit_u8(0x00);
                    buf.emit_u8(*imm);
                }
                _ => panic!("MovMem8Imm8 unsupported register: {:?}", reg),
            }
        }

        AsmInst::CmpMem8Imm8(reg, imm) => {
            // cmp byte ptr [r/m64], imm8
            //
            // only support:
            //   cmp byte ptr [r13 + 0], imm8
            match reg {
                Reg64::R13 => {
                    buf.emit_u8(0x41);
                    buf.emit_u8(0x80);
                    buf.emit_u8(0x7D);
                    buf.emit_u8(0x00);
                    buf.emit_u8(*imm);
                }
                _ => panic!("CmpMem8Imm8 unsupported register: {:?}", reg),
            }
        }

        AsmInst::Jz(label) => {
            // 0F 84 rel32
            buf.emit_u8(0x0F);
            buf.emit_u8(0x84);
            buf.emit_rel32_fixup(*label);
        }

        AsmInst::Jnz(label) => {
            // 0F 85 rel32
            buf.emit_u8(0x0F);
            buf.emit_u8(0x85);
            buf.emit_rel32_fixup(*label);
        }

        AsmInst::Syscall => {
            // just call, but don't check rax
            buf.emit_u8(0x0F);
            buf.emit_u8(0x05);
        }
    }
}
