//! x86_64 machine-code encoder (encode.rs).
//!
//! Converts an `AsmProgram` (assembly IR) into the actual byte sequence of
//! x86_64 machine code.
//!
//! ## Encoding flow
//!
//! 1. **Sequential encoding**: iterate over every `AsmInst` and append the
//!    corresponding x86_64 bytes. Jump / call targets are first written as
//!    zero placeholders with a recorded `Fixup`.
//!
//! 2. **Label binding**: when a `Label` pseudo-instruction is encountered,
//!    record its byte offset.
//!
//! 3. **Fixup resolution**: after every instruction is emitted, walk the
//!    fixup list and for each entry:
//!    - look up the target label's offset,
//!    - compute the relative displacement `rel = target - next_ip`,
//!    - overwrite the 4-byte placeholder with the little-endian `rel32`.
//!
//! ## x86_64 encoding primer
//!
//! A typical x86_64 instruction has up to six optional components:
//!
//! ```text
//! [prefix] [REX] [opcode (1-3 bytes)] [ModRM] [SIB] [displacement] [immediate]
//! ```
//!
//! ### REX prefix (0x40..=0x4F)
//!
//! Format: `0100 WRXB`.
//! - W: 64-bit operand width.
//! - R: extend ModRM.reg (3 → 4 bits, enabling R8..=R15).
//! - X: extend SIB.index.
//! - B: extend ModRM.rm (or the register baked into the opcode).
//!
//! ### ModRM byte
//!
//! Format: `mm_rrr_mmm` (2 + 3 + 3 bits).
//! - `mod` (mm): addressing mode.
//!   - 00 = register indirect (`[reg]`; special case: `rm=101` → `[RIP+disp32]`).
//!   - 01 = register indirect + 8-bit displacement (`[reg+disp8]`).
//!   - 10 = register indirect + 32-bit displacement (`[reg+disp32]`).
//!   - 11 = register direct (the register itself).
//! - `reg` (rrr): register operand or opcode extension (subcode).
//! - `rm` (mmm): register / memory operand (special case: `rm=100` → SIB
//!   byte follows).

use std::collections::HashMap;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};

/// Encoded program, carrying the finished machine-code byte sequence.
#[derive(Debug, Clone)]
pub struct EncodedProgram {
    /// `.text` bytes, ready to be written into an ELF / PE image.
    pub text: Vec<u8>,
}

/// Byte range that a single `AsmInst` occupies inside the emitted `.text`.
///
/// `dump_hex_listing()` slices directly into the real encoder output using
/// this metadata, so there is no need for a parallel debug-only encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EncodedInst {
    /// Start offset of the instruction inside `.text`.
    pub offset: usize,
    /// Encoded byte length. Labels and other pseudo-instructions have length 0.
    pub len: usize,
}

/// Kind of a fixup.
///
/// Two variants cover every near jump, CALL, and RIP-relative helper that the
/// Brainfuck backend emits:
/// - [`FixupKind::Rel32FromNextInsn`] — 4-byte displacement for long Jcc /
///   JMP / CALL.
/// - [`FixupKind::Rel8FromNextInsn`] — 1-byte displacement for the short Jcc
///   / JMP forms introduced by the relaxation pass.
#[derive(Debug, Clone, Copy)]
enum FixupKind {
    /// 32-bit signed relative displacement, relative to the four bytes that
    /// follow the fixup site (i.e. the IP of the next instruction).
    ///
    /// `rel32 = target_offset - (fixup_at + 4)`
    ///
    /// The `+4` reflects that by the time the CPU executes the jump, RIP has
    /// already advanced past the `rel32` field.
    Rel32FromNextInsn,

    /// 8-bit signed relative displacement, relative to the byte that follows
    /// the fixup site.
    ///
    /// `rel8 = target_offset - (fixup_at + 1)`
    ///
    /// Panics in [`CodeBuffer::finish`] when the resolved displacement does
    /// not fit in `i8`. Only the relaxation pass in
    /// [`crate::backend::x86_64::relax`] is allowed to emit this fixup kind,
    /// and only after confirming the jump sits within rel8 range.
    Rel8FromNextInsn,
}

/// A record of a placeholder that still needs to be patched.
///
/// During encoding, jump and call targets are not yet known (forward refs),
/// so four zero bytes are emitted and a `Fixup` is queued. The fixup is
/// resolved once the whole program has been encoded.
#[derive(Debug, Clone, Copy)]
struct Fixup {
    /// Target label of the jump or call.
    label: AsmLabel,

    /// Offset of the placeholder inside `bytes`.
    at: usize,

    /// Fixup kind, which determines how the patched value is computed.
    kind: FixupKind,
}

/// Machine-code encoding buffer.
///
/// Exposes low-level helpers that:
/// - append bytes and integers,
/// - bind labels to the current offset,
/// - record fixups,
/// - resolve all fixups and hand back the finished machine-code buffer.
///
/// Intended usage:
/// ```text
/// 1. Construct a CodeBuffer.
/// 2. For each instruction: call encode_inst to append its bytes.
/// 3. Call finish() to resolve fixups and obtain bytes + label map.
/// ```
struct CodeBuffer {
    /// Machine-code byte buffer.
    bytes: Vec<u8>,

    /// Map of label → byte offset.
    ///
    /// Each time a `Label` pseudo-instruction is encountered during encoding,
    /// the label is bound to the current length of `bytes`.
    labels: HashMap<AsmLabel, usize>,

    /// Fixups awaiting resolution in `finish()`.
    fixups: Vec<Fixup>,
}

impl CodeBuffer {
    /// Create an empty code buffer.
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    /// Return the current write position (i.e. number of bytes emitted so
    /// far), which is also the offset of the next byte.
    fn pos(&self) -> usize {
        self.bytes.len()
    }

    /// Bind a label to the current byte offset.
    ///
    /// Fixup resolution uses this offset as the jump target. Binding the
    /// same label twice is (almost always) a bug and is rejected.
    fn bind_label(&mut self, label: AsmLabel) {
        let previous = self.labels.insert(label, self.pos());
        assert!(
            previous.is_none(),
            "label bound multiple times: {:?}",
            label
        );
    }

    /// Append one byte.
    fn emit_u8(&mut self, b: u8) {
        self.bytes.push(b);
    }

    fn emit_bytes(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
    }

    /// Append a little-endian 32-bit signed integer.
    ///
    /// x86_64 is little-endian: the least-significant byte sits at the lowest
    /// address.
    fn emit_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// Append a little-endian 64-bit signed integer.
    fn emit_i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// Emit a 4-byte zero placeholder and record the matching fixup.
    ///
    /// This is the core helper for every long jump / call: reserve space now,
    /// patch in the real displacement later.
    fn emit_rel32_fixup(&mut self, label: AsmLabel) {
        let at = self.pos();
        self.emit_i32(0); // four-byte zero placeholder
        self.fixups.push(Fixup {
            label,
            at,
            kind: FixupKind::Rel32FromNextInsn,
        });
    }

    /// Emit a 1-byte zero placeholder for the short Jcc / JMP forms and
    /// record the matching rel8 fixup.
    fn emit_rel8_fixup(&mut self, label: AsmLabel) {
        let at = self.pos();
        self.emit_u8(0); // one-byte zero placeholder
        self.fixups.push(Fixup {
            label,
            at,
            kind: FixupKind::Rel8FromNextInsn,
        });
    }

    /// Finalise encoding: apply every fixup and return the machine-code
    /// buffer along with the label → offset map.
    ///
    /// ## Fixup algorithm (`Rel32FromNextInsn`)
    ///
    /// 1. Look up the target label's offset in `labels`.
    /// 2. Next-instruction IP: `next_ip = fixup.at + 4`.
    /// 3. Displacement: `rel = target - next_ip`.
    /// 4. Verify `rel` fits in `i32` (a ±2 GiB range).
    /// 5. Write the little-endian `rel32` into
    ///    `bytes[fixup.at..fixup.at + 4]`.
    ///
    /// ## Panics
    ///
    /// - Jump to a label that was never bound.
    /// - Displacement does not fit in `i32` (requires a `.text` larger than
    ///   2 GiB, which is well outside anything this backend produces).
    fn finish(mut self) -> (Vec<u8>, HashMap<AsmLabel, usize>) {
        for fixup in &self.fixups {
            // Look up the target label offset.
            let target = *self
                .labels
                .get(&fixup.label)
                .unwrap_or_else(|| panic!("unknown label: {:?}", fixup.label))
                as i64;

            match fixup.kind {
                FixupKind::Rel32FromNextInsn => {
                    // Next-instruction IP = fixup site + 4 (the rel32 field width).
                    let next_ip = (fixup.at + 4) as i64;
                    let rel = target - next_ip;
                    let rel32 = i32::try_from(rel).expect("rel32 out of range");
                    self.bytes[fixup.at..fixup.at + 4].copy_from_slice(&rel32.to_le_bytes());
                }
                FixupKind::Rel8FromNextInsn => {
                    // Next-instruction IP = fixup site + 1 (the rel8 field width).
                    let next_ip = (fixup.at + 1) as i64;
                    let rel = target - next_ip;
                    let rel8 = i8::try_from(rel)
                        .unwrap_or_else(|_| panic!("rel8 out of range for {:?}", fixup.label));
                    self.bytes[fixup.at] = rel8 as u8;
                }
            }
        }

        (self.bytes, self.labels)
    }
}

/// Encode an `AsmProgram` into machine code.
///
/// Entry point of this module; called by `x86_64/mod.rs`.
pub fn encode_program(program: &AsmProgram) -> EncodedProgram {
    encode_program_with_inst_map(program).0
}

/// Encode an `AsmProgram` and return both the encoded bytes and a map from
/// each [`AsmLabel`] to its resolved byte offset inside `.text`.
pub(crate) fn encode_program_with_labels(
    program: &AsmProgram,
) -> (EncodedProgram, HashMap<AsmLabel, usize>) {
    let mut buf = CodeBuffer::new();
    for inst in &program.insts {
        encode_inst(&mut buf, inst);
    }
    let (text, labels) = buf.finish();
    (EncodedProgram { text }, labels)
}

/// Encode an `AsmProgram` into machine code, preserving the byte range of
/// every input instruction inside `.text`.
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

    let (text, _labels) = buf.finish();
    (EncodedProgram { text }, inst_map)
}

/// Return the x86_64 register number for a `Reg64`.
///
/// The low three bits go into ModRM / SIB / opcode fields; the fourth bit
/// feeds the REX R/B/X extension. Note that this numbering skips nothing —
/// even registers this backend never references are assigned their canonical
/// x86 numbers.
fn reg_num(reg: Reg64) -> u8 {
    match reg {
        Reg64::Rax => 0,  // 000
        Reg64::Rcx => 1,  // 001
        Reg64::Rdx => 2,  // 010
        Reg64::Rbx => 3,  // 011
        Reg64::Rsp => 4,  // 100
        Reg64::Rbp => 5,  // 101
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

/// Emit a REX.W prefix byte.
///
/// REX byte format: `0100 WRXB`.
///
/// `r`, `x`, `b` are the single bits of the R / X / B fields:
/// - `r`: extension of ModRM.reg → pass `reg_num >> 3`.
/// - `x`: extension of SIB.index → usually `0` (this backend does not use
///   SIB beyond a single fixed case).
/// - `b`: extension of ModRM.rm  → pass `rm_num >> 3`.
///
/// Base value `0x48 = 0100_1000` means `W=1` (64-bit operand width) with all
/// other bits clear.
fn emit_rex_w(buf: &mut CodeBuffer, r: u8, x: u8, b: u8) {
    let rex = 0x48 | ((r & 1) << 2) | ((x & 1) << 1) | (b & 1);
    buf.emit_u8(rex);
}

/// Emit `op reg, sign-extended immediate` (register + immediate, 64-bit
/// operand width).
///
/// The on-wire encoding is width-adaptive: when `imm` fits in a signed 8-bit
/// range the encoder emits the shorter `REX.W + 0x83 + ModRM + imm8` form
/// (4 bytes), otherwise it falls back to `REX.W + 0x81 + ModRM + imm32`
/// (7 bytes). The CPU sign-extends either immediate to 64 bits, so the
/// semantics are identical; this is a pure code-size optimisation.
///
/// `subcode` selects the opcode extension (ModRM.reg field):
/// - 0 → ADD
/// - 1 → OR
/// - 4 → AND
/// - 5 → SUB
/// - 7 → CMP
fn emit_reg_imm32(buf: &mut CodeBuffer, subcode: u8, reg: Reg64, imm: i32) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    if let Ok(imm8) = i8::try_from(imm) {
        buf.emit_u8(0x83); // opcode: r/m64, sign-extended imm8 form
        buf.emit_u8(0b11_000_000 | ((subcode & 7) << 3) | (rm & 7));
        buf.emit_u8(imm8 as u8);
    } else {
        buf.emit_u8(0x81); // opcode: r/m64, imm32 form
        buf.emit_u8(0b11_000_000 | ((subcode & 7) << 3) | (rm & 7));
        buf.emit_i32(imm);
    }
}

fn emit_modrm_sib_mem(buf: &mut CodeBuffer, reg_field: u8, base: Reg64, disp: i32) {
    let base_num = reg_num(base);
    let rm_low3 = base_num & 7;
    let needs_sib = rm_low3 == 4;
    let fits_disp8 = i8::try_from(disp).is_ok();
    let force_disp32 = !fits_disp8;

    let mod_bits = if disp == 0 && rm_low3 != 5 && !force_disp32 {
        0b00
    } else if !force_disp32 {
        0b01
    } else {
        0b10
    };

    buf.emit_u8((mod_bits << 6) | ((reg_field & 7) << 3) | if needs_sib { 4 } else { rm_low3 });
    if needs_sib {
        buf.emit_u8(0b00_100_100);
    }
    match mod_bits {
        0b00 => {}
        0b01 => buf.emit_u8(disp as i8 as u8),
        0b10 => buf.emit_i32(disp),
        _ => unreachable!(),
    }
}

fn emit_mem_reg64(buf: &mut CodeBuffer, opcode: u8, base: Reg64, disp: i32, reg: Reg64) {
    emit_rex_w(buf, reg_num(reg) >> 3, 0, reg_num(base) >> 3);
    buf.emit_u8(opcode);
    emit_modrm_sib_mem(buf, reg_num(reg), base, disp);
}

fn emit_lea_reg_mem(buf: &mut CodeBuffer, dst: Reg64, base: Reg64, disp: i32) {
    emit_rex_w(buf, reg_num(dst) >> 3, 0, reg_num(base) >> 3);
    buf.emit_u8(0x8D);
    emit_modrm_sib_mem(buf, reg_num(dst), base, disp);
}

fn emit_rip_rel(buf: &mut CodeBuffer, opcode: u8, subcode_or_reg: u8, label: AsmLabel, rex_r: u8) {
    emit_rex_w(buf, rex_r, 0, 0);
    buf.emit_u8(opcode);
    buf.emit_u8(0b00_000_101 | ((subcode_or_reg & 7) << 3));
    buf.emit_rel32_fixup(label);
}

/// Emit a logical-right-shift instruction: `shr reg, imm8`.
///
/// Encoding: `REX.W + 0xC1 + ModRM(11, 5, rm) + imm8`.
///
/// `ModRM.reg = 5` selects SHR in the shift group (4 = SHL, 5 = SHR,
/// 7 = SAR).
fn emit_shift_right_imm8(buf: &mut CodeBuffer, reg: Reg64, imm: u8) {
    let rm = reg_num(reg);
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(0xC1); // shift group (imm8 operand)
    buf.emit_u8(0b11_000_000 | (5 << 3) | (rm & 7)); // subcode=5 → SHR
    buf.emit_u8(imm);
}

fn emit_mem8_disp0(buf: &mut CodeBuffer, opcode: u8, subcode: u8, reg: Reg64, imm: u8) {
    emit_mem8_disp0_no_imm(buf, opcode, subcode, reg);
    buf.emit_u8(imm);
}

/// Emit `REX + opcode + ModRM(01, subcode, rm) + disp8 + imm8` — byte ALU /
/// mov with a signed 8-bit displacement.
///
/// Used by the LIR `lir_postpone` pass's displacement forms. Same SIB-free
/// restriction as [`emit_mem8_disp0`] — the encoder panics if the base
/// register needs a SIB byte (low three bits == 4). Codegen hard-pins the
/// base to R13, so this never fires in practice.
fn emit_mem8_disp8(buf: &mut CodeBuffer, opcode: u8, subcode: u8, reg: Reg64, disp: i8, imm: u8) {
    let rm = reg_num(reg);
    assert!(
        (rm & 7) != 4,
        "mem8 disp8 encoding requires SIB for register {:?}",
        reg
    );
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(opcode);
    buf.emit_u8(0b01_000_000 | ((subcode & 7) << 3) | (rm & 7));
    buf.emit_u8(disp as u8);
    buf.emit_u8(imm);
}

/// Emit `REX + opcode + ModRM(10, subcode, rm) + disp32 + imm8` — byte ALU /
/// mov with a signed 32-bit displacement.
///
/// Disp32 counterpart of [`emit_mem8_disp8`], selected when the offset does
/// not fit a signed byte. Same SIB-free restriction.
fn emit_mem8_disp32(buf: &mut CodeBuffer, opcode: u8, subcode: u8, reg: Reg64, disp: i32, imm: u8) {
    let rm = reg_num(reg);
    assert!(
        (rm & 7) != 4,
        "mem8 disp32 encoding requires SIB for register {:?}",
        reg
    );
    emit_rex_w(buf, 0, 0, rm >> 3);
    buf.emit_u8(opcode);
    buf.emit_u8(0b10_000_000 | ((subcode & 7) << 3) | (rm & 7));
    for byte in disp.to_le_bytes() {
        buf.emit_u8(byte);
    }
    buf.emit_u8(imm);
}

/// Emit `REX + opcode + ModRM(01, subcode, rm) + disp8(0)` without an
/// immediate byte.
///
/// Used by opcodes whose r/m form takes no immediate operand (e.g.
/// `inc`/`dec byte [reg]` via the `0xFE` group). Same SIB-free restriction as
/// [`emit_mem8_disp0`]: the low three bits of the register number must not be
/// `4` because the encoder does not emit a SIB byte. The `mod=01 + disp8=0`
/// form dodges the `[RIP+disp32]` aliasing that `mod=00 + rm&7==5` would
/// otherwise trigger (relevant for R13).
fn emit_mem8_disp0_no_imm(buf: &mut CodeBuffer, opcode: u8, subcode: u8, reg: Reg64) {
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
}

/// Emit a conditional near jump: `0F cc rel32`.
///
/// Every near Jcc uses this encoding, with `cc` selecting the condition. The
/// `rel32` is relative to the IP of the instruction that follows.
fn emit_jcc_rel32(buf: &mut CodeBuffer, cc: u8, label: AsmLabel) {
    buf.emit_u8(0x0F); // two-byte opcode prefix
    buf.emit_u8(cc); // condition code
    buf.emit_rel32_fixup(label); // 32-bit relative displacement (fixup)
}

/// Emit a short conditional / unconditional jump: `opcode rel8`.
///
/// The short Jcc and JMP forms share this single-byte-opcode + single-byte
/// displacement shape. Only the relaxation pass emits these variants, after
/// verifying the target sits within rel8 range.
fn emit_short_jump(buf: &mut CodeBuffer, opcode: u8, label: AsmLabel) {
    buf.emit_u8(opcode);
    buf.emit_rel8_fixup(label);
}

/// Emit `n` bytes of NOP padding using Intel-recommended multi-byte NOP forms.
fn emit_nop_sequence(buf: &mut CodeBuffer, mut n: usize) {
    while n > 0 {
        match n {
            1 => {
                buf.emit_u8(0x90);
                n -= 1;
            }
            2 => {
                buf.emit_u8(0x66);
                buf.emit_u8(0x90);
                n -= 2;
            }
            3 => {
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x00);
                n -= 3;
            }
            4 => {
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x40);
                buf.emit_u8(0x00);
                n -= 4;
            }
            5 => {
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x44);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                n -= 5;
            }
            6 => {
                buf.emit_u8(0x66);
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x44);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                n -= 6;
            }
            7 => {
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x80);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                n -= 7;
            }
            _ => {
                // 8-byte NOP: 0F 1F 84 00 00 00 00 00
                buf.emit_u8(0x0F);
                buf.emit_u8(0x1F);
                buf.emit_u8(0x84);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                buf.emit_u8(0x00);
                n -= 8;
            }
        }
    }
}

/// Encode a single `AsmInst` into the code buffer.
///
/// Per-instruction encoding details live in the matching match arm.
fn encode_inst(buf: &mut CodeBuffer, inst: &AsmInst) {
    match inst {
        // Pseudo-instructions.
        AsmInst::Label(label) => {
            buf.bind_label(*label);
        }

        AsmInst::Align16 => {
            let pad = (16 - (buf.pos() % 16)) % 16;
            emit_nop_sequence(buf, pad);
        }

        // mov r64, imm64.
        //
        // The only instruction that loads a full 64-bit immediate.
        // Encoding: REX.W + (0xB8 + rd) + imm64. The low three bits of the
        // opcode carry the destination register number. Total length: 10
        // bytes (1 REX + 1 opcode + 8 imm64).
        AsmInst::MovRegImm64(reg, imm) => {
            let code = reg_num(*reg);
            emit_rex_w(buf, 0, 0, code >> 3);
            buf.emit_u8(0xB8 + (code & 7));
            buf.emit_i64(*imm);
        }

        // mov dst, src.
        //
        // Encoding: REX.W + 0x89 + ModRM(11, src, dst). 0x89 is the r → r/m
        // direction, so ModRM.reg = src and ModRM.rm = dst.
        AsmInst::MovRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x89);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // add r64, imm32.
        AsmInst::AddRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 0, *reg, *imm); // subcode 0 = ADD
        }

        AsmInst::AndRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 4, *reg, *imm); // subcode 4 = AND
        }

        // add dst, src.
        //
        // Encoding: REX.W + 0x01 + ModRM(11, src, dst). 0x01 is the opcode
        // for ADD r/m64, r64.
        AsmInst::AddRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x01);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // sub dst, src.
        //
        // Encoding: REX.W + 0x29 + ModRM(11, src, dst). 0x29 is the opcode
        // for SUB r/m64, r64.
        AsmInst::SubRegReg(dst, src) => {
            emit_rex_w(buf, reg_num(*src) >> 3, 0, reg_num(*dst) >> 3);
            buf.emit_u8(0x29);
            buf.emit_u8(0b11_000_000 | ((reg_num(*src) & 7) << 3) | (reg_num(*dst) & 7));
        }

        // cmp lhs, rhs.
        //
        // Encoding: REX.W + 0x39 + ModRM(11, rhs, lhs). Semantically computes
        // `lhs - rhs`, updates EFLAGS, discards the result.
        AsmInst::CmpRegReg(lhs, rhs) => {
            emit_rex_w(buf, reg_num(*rhs) >> 3, 0, reg_num(*lhs) >> 3);
            buf.emit_u8(0x39);
            buf.emit_u8(0b11_000_000 | ((reg_num(*rhs) & 7) << 3) | (reg_num(*lhs) & 7));
        }

        // cmp r64, imm32.
        AsmInst::CmpRegImm32(reg, imm) => {
            emit_reg_imm32(buf, 7, *reg, *imm); // subcode 7 = CMP
        }

        // shr r64, imm8.
        AsmInst::ShrRegImm8(reg, imm) => emit_shift_right_imm8(buf, *reg, *imm),

        AsmInst::LeaRegMem(dst, base, disp) => emit_lea_reg_mem(buf, *dst, *base, *disp),

        AsmInst::LeaRegLabel(dst, label) => {
            emit_rip_rel(buf, 0x8D, reg_num(*dst), *label, reg_num(*dst) >> 3);
        }

        AsmInst::MovMemReg64(base, disp, src) => {
            emit_mem_reg64(buf, 0x89, *base, *disp, *src);
        }

        AsmInst::MovRegMem64(dst, base, disp) => {
            emit_mem_reg64(buf, 0x8B, *base, *disp, *dst);
        }

        // add byte ptr [reg+0], imm8.
        //
        // Encoding: REX.W + 0x80 + ModRM(01, 0, rm) + disp8(0) + imm8.
        // 0x80 is the byte ALU group, with ModRM.reg = 0 selecting ADD.
        //
        // Why `mod=01` (i.e. [reg+disp8]) instead of `mod=00` (i.e. [reg]):
        // when `rm & 7 == 5` (the low three bits of R13) the CPU interprets
        // `mod=00` as `[RIP+disp32]` rather than `[r13]`. Using
        // `mod=01 + disp8=0` dodges that ambiguity.
        //
        // Caveat: when `rm & 7 == 4` (e.g. R12) the CPU expects a SIB byte
        // after ModRM, which this encoder does not emit. Safe in practice
        // because codegen only issues this form with R13.
        AsmInst::AddMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0x80, 0, *reg, *imm as u8);
        }

        // inc byte ptr [reg+0].
        //
        // Encoding: REX.W + 0xFE + ModRM(01, 0, rm) + disp8(0). 0xFE is the
        // byte INC/DEC group, with ModRM.reg = 0 selecting INC.
        AsmInst::IncMem8(reg) => {
            emit_mem8_disp0_no_imm(buf, 0xFE, 0, *reg);
        }

        // dec byte ptr [reg+0].
        //
        // Encoding: REX.W + 0xFE + ModRM(01, 1, rm) + disp8(0). Same byte
        // INC/DEC group as INC, with ModRM.reg = 1 selecting DEC.
        AsmInst::DecMem8(reg) => {
            emit_mem8_disp0_no_imm(buf, 0xFE, 1, *reg);
        }

        // mov byte ptr [reg+0], imm8.
        //
        // Encoding: REX.W + 0xC6 + ModRM(01, 0, rm) + disp8(0) + imm8.
        // 0xC6 is the opcode for MOV r/m8, imm8.
        AsmInst::MovMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0xC6, 0, *reg, *imm);
        }

        // add byte ptr [reg + disp8], imm8.
        //
        // Encoding: REX.W + 0x80 + ModRM(01, 0, rm) + disp8 + imm8.
        // Same byte ALU group / subcode as AddMem8Imm8; only ModRM.disp8
        // changes. Same SIB-free restriction.
        AsmInst::AddMem8ImmDisp8(reg, disp, imm) => {
            emit_mem8_disp8(buf, 0x80, 0, *reg, *disp, *imm as u8);
        }

        // mov byte ptr [reg + disp8], imm8.
        //
        // Encoding: REX.W + 0xC6 + ModRM(01, 0, rm) + disp8 + imm8.
        AsmInst::MovMem8ImmDisp8(reg, disp, imm) => {
            emit_mem8_disp8(buf, 0xC6, 0, *reg, *disp, *imm);
        }

        // add byte ptr [reg + disp32], imm8.
        //
        // Encoding: REX.W + 0x80 + ModRM(10, 0, rm) + disp32 + imm8.
        AsmInst::AddMem8ImmDisp32(reg, disp, imm) => {
            emit_mem8_disp32(buf, 0x80, 0, *reg, *disp, *imm as u8);
        }

        // mov byte ptr [reg + disp32], imm8.
        //
        // Encoding: REX.W + 0xC6 + ModRM(10, 0, rm) + disp32 + imm8.
        AsmInst::MovMem8ImmDisp32(reg, disp, imm) => {
            emit_mem8_disp32(buf, 0xC6, 0, *reg, *disp, *imm);
        }

        // cmp byte ptr [reg+0], imm8.
        //
        // Encoding: REX.W + 0x80 + ModRM(01, 7, rm) + disp8(0) + imm8.
        // ModRM.reg = 7 selects CMP in the byte ALU group.
        AsmInst::CmpMem8Imm8(reg, imm) => {
            emit_mem8_disp0(buf, 0x80, 7, *reg, *imm);
        }

        // Conditional near jumps: 0x0F + cc + rel32.
        // D5: segment-override prefixes 0x2E (not-taken) and 0x3E (taken)
        // are parsed by AMD as static branch hints. Intel ignores them since
        // Netburst, and both shapes are legal no-op prefixes on either
        // vendor. BF `[` is an unlikely jump (we almost always enter the
        // loop body) so `Jz` takes 0x2E; BF `]` is a likely jump (we keep
        // looping) so `Jnz` takes 0x3E.
        AsmInst::Jz(label) => {
            buf.emit_u8(0x2E); // branch-not-taken hint
            emit_jcc_rel32(buf, 0x84, *label); // ZF=1
        }
        AsmInst::Jnz(label) => {
            buf.emit_u8(0x3E); // branch-taken hint
            emit_jcc_rel32(buf, 0x85, *label); // ZF=0
        }
        AsmInst::Jb(label) => emit_jcc_rel32(buf, 0x82, *label), // CF=1 (unsigned <)
        AsmInst::Jae(label) => emit_jcc_rel32(buf, 0x83, *label), // CF=0 (unsigned >=)
        AsmInst::Jl(label) => emit_jcc_rel32(buf, 0x8C, *label), // SF≠OF (signed <)
        AsmInst::Jge(label) => emit_jcc_rel32(buf, 0x8D, *label), // SF=OF (signed >=)

        // Unconditional near jump: 0xE9 + rel32.
        AsmInst::Jmp(label) => {
            buf.emit_u8(0xE9);
            buf.emit_rel32_fixup(*label);
        }

        // Short conditional / unconditional jumps: single-byte opcode + rel8.
        // Produced only by the relaxation pass in
        // `crate::backend::x86_64::relax`.
        AsmInst::JzShort(label) => {
            buf.emit_u8(0x2E); // branch-not-taken hint (D5)
            emit_short_jump(buf, 0x74, *label);
        }
        AsmInst::JnzShort(label) => {
            buf.emit_u8(0x3E); // branch-taken hint (D5)
            emit_short_jump(buf, 0x75, *label);
        }
        AsmInst::JbShort(label) => emit_short_jump(buf, 0x72, *label),
        AsmInst::JaeShort(label) => emit_short_jump(buf, 0x73, *label),
        AsmInst::JlShort(label) => emit_short_jump(buf, 0x7C, *label),
        AsmInst::JgeShort(label) => emit_short_jump(buf, 0x7D, *label),
        AsmInst::JmpShort(label) => emit_short_jump(buf, 0xEB, *label),

        // Near call: 0xE8 + rel32. CALL implicitly pushes RIP then jumps.
        AsmInst::Call(label) => {
            buf.emit_u8(0xE8);
            buf.emit_rel32_fixup(*label);
        }

        AsmInst::CallMemLabel(label) => {
            emit_rip_rel(buf, 0xFF, 2, *label, 0);
        }

        // Near return: 0xC3. RET implicitly pops RIP.
        AsmInst::Ret => buf.emit_u8(0xC3),

        // Clear direction flag: 0xFC.
        AsmInst::Cld => buf.emit_u8(0xFC),

        // rep movsb: 0xF3 (REP prefix) + 0xA4 (MOVSB).
        // Repeats rcx times: byte [rdi++] = byte [rsi++].
        AsmInst::RepMovsb => {
            buf.emit_u8(0xF3);
            buf.emit_u8(0xA4);
        }

        // Set direction flag: 0xFD.
        AsmInst::Std => buf.emit_u8(0xFD),

        // repne scasb: 0xF2 (REPNE prefix) + 0xAE (SCASB).
        // Compares al with [rdi], post-{inc,dec}rements rdi (sign by DF),
        // decrements rcx; stops when ZF=1 (match) or rcx==0.
        AsmInst::RepneScasb => {
            buf.emit_u8(0xF2);
            buf.emit_u8(0xAE);
        }

        // xor eax, eax: 0x31 0xC0 — zero RAX (upper 32 bits also cleared).
        AsmInst::XorEaxEax => {
            buf.emit_u8(0x31);
            buf.emit_u8(0xC0);
        }

        // mov ecx, imm32: 0xB9 + imm32 (5 bytes; upper 32 bits of RCX cleared).
        AsmInst::MovEcxImm32(imm) => {
            buf.emit_u8(0xB9);
            buf.emit_i32(*imm);
        }

        // rep stosb: 0xF3 (REP prefix) + 0xAA (STOSB).
        // Stores al into [rdi], post-{inc,dec}rements rdi (DF), decrements rcx;
        // continues until rcx == 0.
        AsmInst::RepStosb => {
            buf.emit_u8(0xF3);
            buf.emit_u8(0xAA);
        }

        // syscall: 0x0F 0x05.
        AsmInst::Syscall => {
            buf.emit_u8(0x0F);
            buf.emit_u8(0x05);
        }

        AsmInst::RawBytes(bytes) => {
            buf.emit_bytes(bytes);
        }

        AsmInst::Push(reg) => emit_push_pop64(buf, *reg, true),
        AsmInst::Pop(reg) => emit_push_pop64(buf, *reg, false),

        // movzx ebx, byte [r13+0]
        AsmInst::MovzxEbxFromMemR13 => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x0f);
            buf.emit_u8(0xb6);
            buf.emit_u8(0x5d);
            buf.emit_u8(0x00);
        }

        // imul eax, ebx, imm32
        AsmInst::ImulEaxEbxImm32(imm) => {
            buf.emit_u8(0x69);
            buf.emit_u8(0xc3);
            buf.emit_i32(*imm);
        }

        // add byte [r13+0], al
        AsmInst::AddMemR13Al => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x00);
            buf.emit_u8(0x45);
            buf.emit_u8(0x00);
        }

        // add byte [r13+0], bl
        //   REX.B (0x41) + 0x00 + ModRM(mod=01, reg=bl=3, rm=r13 low3=5) + disp8(0)
        //   = 41 00 5D 00
        AsmInst::AddMemR13Bl => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x00);
            buf.emit_u8(0x5D);
            buf.emit_u8(0x00);
        }

        // sub byte [r13+0], bl
        //   REX.B (0x41) + 0x28 + ModRM(mod=01, reg=bl=3, rm=r13 low3=5) + disp8(0)
        //   = 41 28 5D 00
        AsmInst::SubMemR13Bl => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x28);
            buf.emit_u8(0x5D);
            buf.emit_u8(0x00);
        }

        // add byte [r13+disp8], bl
        //   REX.B (0x41) + 0x00 + ModRM(mod=01, reg=bl=3, rm=r13 low3=5) + disp8
        AsmInst::AddMemR13BlDisp8(disp) => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x00);
            buf.emit_u8(0x5D);
            buf.emit_u8(*disp as u8);
        }

        // sub byte [r13+disp8], bl
        //   REX.B (0x41) + 0x28 + ModRM(mod=01, reg=bl=3, rm=r13 low3=5) + disp8
        AsmInst::SubMemR13BlDisp8(disp) => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x28);
            buf.emit_u8(0x5D);
            buf.emit_u8(*disp as u8);
        }

        // add byte [r13+disp8], al
        //   REX.B (0x41) + 0x00 + ModRM(mod=01, reg=al=0, rm=r13 low3=5) + disp8
        AsmInst::AddMemR13AlDisp8(disp) => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x00);
            buf.emit_u8(0x45);
            buf.emit_u8(*disp as u8);
        }

        // mov al, byte [r13+0]
        //   REX.B (0x41) + 0x8A + ModRM(mod=01, reg=al=0, rm=r13 low3=5) + disp8(0)
        //   = 41 8A 45 00
        AsmInst::MovAlMemR13 => {
            buf.emit_u8(0x41);
            buf.emit_u8(0x8A);
            buf.emit_u8(0x45);
            buf.emit_u8(0x00);
        }

        // mov byte [rbx], al
        //   No REX (all low regs, byte op on rbx is safe).
        //   0x88 + ModRM(mod=00, reg=al=0, rm=rbx=3)
        //   = 88 03
        AsmInst::MovMemRbxAl => {
            buf.emit_u8(0x88);
            buf.emit_u8(0x03);
        }
    }
}

fn emit_push_pop64(buf: &mut CodeBuffer, reg: Reg64, is_push: bool) {
    let n = reg_num(reg);
    let opc = if is_push { 0x50 } else { 0x58 };
    if n >= 8 {
        buf.emit_u8(0x41);
    }
    buf.emit_u8(opc + (n & 7));
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

    #[test]
    fn lea_rsp_disp8_and_stack_load_store_encode_with_sib() {
        let program = AsmProgram {
            insts: vec![
                AsmInst::LeaRegMem(Reg64::R9, Reg64::Rsp, 24),
                AsmInst::MovMemReg64(Reg64::Rsp, 32, Reg64::Rax),
                AsmInst::MovRegMem64(Reg64::R11, Reg64::Rsp, 16),
            ],
        };

        let encoded = encode_program(&program);
        assert_eq!(
            encoded.text,
            vec![
                0x4c, 0x8d, 0x4c, 0x24, 0x18, //
                0x48, 0x89, 0x44, 0x24, 0x20, //
                0x4c, 0x8b, 0x5c, 0x24, 0x10, //
            ]
        );
    }

    #[test]
    fn inc_dec_mem8_on_r13_encode_to_four_bytes() {
        let program = AsmProgram {
            insts: vec![AsmInst::IncMem8(Reg64::R13), AsmInst::DecMem8(Reg64::R13)],
        };
        let encoded = encode_program(&program);
        // REX.W|B=0x49, opcode 0xFE, ModRM(mod=01 reg=0/1 rm=5)=0x45/0x4D, disp8=0x00.
        assert_eq!(
            encoded.text,
            vec![
                0x49, 0xFE, 0x45, 0x00, //
                0x49, 0xFE, 0x4D, 0x00,
            ]
        );
    }

    #[test]
    fn reg_imm_uses_imm8_form_for_small_values() {
        // add rax, 1: REX.W + 0x83 /0 + ModRM + imm8 = 4 bytes.
        let program = AsmProgram {
            insts: vec![AsmInst::AddRegImm32(Reg64::Rax, 1)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 4);
        assert_eq!(encoded.text, vec![0x48, 0x83, 0xC0, 0x01]);
    }

    #[test]
    fn reg_imm_uses_imm8_form_for_negative_small_values() {
        // sub via AddRegImm32(-1): REX.W + 0x83 /0 + ModRM + 0xFF = 4 bytes.
        let program = AsmProgram {
            insts: vec![AsmInst::AddRegImm32(Reg64::Rax, -1)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 4);
        assert_eq!(encoded.text[3], 0xFF);
    }

    #[test]
    fn reg_imm_falls_back_to_imm32_for_large_values() {
        // add rax, 0x1000: REX.W + 0x81 /0 + ModRM + imm32 = 7 bytes.
        let program = AsmProgram {
            insts: vec![AsmInst::AddRegImm32(Reg64::Rax, 0x1000)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 7);
        assert_eq!(encoded.text, vec![0x48, 0x81, 0xC0, 0x00, 0x10, 0x00, 0x00]);
    }

    #[test]
    fn reg_imm_imm8_boundary_128_uses_imm32_form() {
        // +128 does not fit i8 (range -128..=127), so falls back to imm32.
        let program = AsmProgram {
            insts: vec![AsmInst::AddRegImm32(Reg64::Rax, 128)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 7);
    }

    #[test]
    fn cmp_reg_imm_also_uses_imm8_when_small() {
        let program = AsmProgram {
            insts: vec![AsmInst::CmpRegImm32(Reg64::Rax, 0)],
        };
        let encoded = encode_program(&program);
        // REX.W + 0x83 /7 + ModRM + imm8 = 4 bytes.
        assert_eq!(encoded.text.len(), 4);
        assert_eq!(encoded.text[1], 0x83);
    }

    #[test]
    fn add_mem8_imm_disp8_on_r13_positive_disp() {
        // add byte [r13 + 5], 3
        // REX.W|B=0x49, opcode=0x80, ModRM(mod=01 reg=0 rm=5)=0x45,
        // disp8=0x05, imm8=0x03.
        let program = AsmProgram {
            insts: vec![AsmInst::AddMem8ImmDisp8(Reg64::R13, 5, 3)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x49, 0x80, 0x45, 0x05, 0x03]);
    }

    #[test]
    fn add_mem8_imm_disp8_on_r13_negative_disp_sign_extends() {
        // add byte [r13 - 1], 1
        // disp8=0xFF (two's complement −1).
        let program = AsmProgram {
            insts: vec![AsmInst::AddMem8ImmDisp8(Reg64::R13, -1, 1)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x49, 0x80, 0x45, 0xFF, 0x01]);
    }

    #[test]
    fn mov_mem8_imm_disp8_on_r13_max_positive_disp() {
        // mov byte [r13 + 127], 0x41
        // REX.W|B=0x49, opcode=0xC6, ModRM=0x45, disp8=0x7F, imm8=0x41.
        let program = AsmProgram {
            insts: vec![AsmInst::MovMem8ImmDisp8(Reg64::R13, 127, 0x41)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x49, 0xC6, 0x45, 0x7F, 0x41]);
    }

    #[test]
    #[should_panic(expected = "mem8 disp8 encoding requires SIB")]
    fn mem8_disp8_rejects_r12_without_sib_support() {
        let program = AsmProgram {
            insts: vec![AsmInst::AddMem8ImmDisp8(Reg64::R12, 1, 1)],
        };
        let _ = encode_program(&program);
    }

    #[test]
    fn add_mem8_imm_disp32_on_r13_encodes_little_endian_disp() {
        // add byte [r13 + 1000], 3
        // REX.W|B=0x49, opcode=0x80, ModRM(mod=10 reg=0 rm=5)=0x85,
        // disp32=0xE8 0x03 0x00 0x00 (1000 LE), imm8=0x03.
        let program = AsmProgram {
            insts: vec![AsmInst::AddMem8ImmDisp32(Reg64::R13, 1000, 3)],
        };
        let encoded = encode_program(&program);
        assert_eq!(
            encoded.text,
            vec![0x49, 0x80, 0x85, 0xE8, 0x03, 0x00, 0x00, 0x03]
        );
    }

    #[test]
    fn mov_mem8_imm_disp32_on_r13_encodes_negative_disp_two_complement() {
        // mov byte [r13 - 1000], 0x41
        // disp32 = 0xFFFFFC18 (−1000 as i32 LE = 0x18 0xFC 0xFF 0xFF).
        let program = AsmProgram {
            insts: vec![AsmInst::MovMem8ImmDisp32(Reg64::R13, -1000, 0x41)],
        };
        let encoded = encode_program(&program);
        assert_eq!(
            encoded.text,
            vec![0x49, 0xC6, 0x85, 0x18, 0xFC, 0xFF, 0xFF, 0x41]
        );
    }

    #[test]
    fn call_mem_label_and_label_map_use_rip_relative_fixups() {
        let slot = AsmLabel(9);
        let program = AsmProgram {
            insts: vec![
                AsmInst::CallMemLabel(slot),
                AsmInst::Ret,
                AsmInst::Label(slot),
                AsmInst::RawBytes(vec![0; 8]),
            ],
        };

        let (encoded, labels) = encode_program_with_labels(&program);
        assert_eq!(labels.get(&slot), Some(&8usize));
        assert_eq!(
            encoded.text[..7],
            [0x48, 0xff, 0x15, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn mov_al_mem_r13_uses_disp8_zero_to_avoid_rip_alias() {
        // mov al, byte [r13] — R13's low 3 bits = 5 would collide with
        // RIP-relative in mod=00, so the encoder must select
        // mod=01 + disp8=0.  Expected: 41 8A 45 00.
        let program = AsmProgram {
            insts: vec![AsmInst::MovAlMemR13],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x8A, 0x45, 0x00]);
    }

    #[test]
    fn mov_mem_rbx_al_encodes_without_rex() {
        // mov byte [rbx], al — no REX (low regs, byte op on rbx is safe).
        // Expected: 88 03  (opcode=0x88, ModRM mod=00 reg=0 rm=3).
        let program = AsmProgram {
            insts: vec![AsmInst::MovMemRbxAl],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x88, 0x03]);
    }

    #[test]
    fn std_encodes_to_single_byte_fd() {
        // std — set direction flag, opcode 0xFD, no operands.
        let program = AsmProgram {
            insts: vec![AsmInst::Std],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0xFD]);
    }

    #[test]
    fn repne_scasb_encodes_to_f2_ae() {
        // repne scasb — REPNE prefix 0xF2, then SCASB opcode 0xAE.
        let program = AsmProgram {
            insts: vec![AsmInst::RepneScasb],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0xF2, 0xAE]);
    }

    #[test]
    fn xor_eax_eax_encodes_to_two_bytes_31_c0() {
        // xor eax, eax — 2-byte form replacing the 10-byte `mov rax, 0`.
        let program = AsmProgram {
            insts: vec![AsmInst::XorEaxEax],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x31, 0xC0]);
    }

    #[test]
    fn mov_ecx_imm32_encodes_with_imm_little_endian() {
        // mov ecx, 0x12345678 — 0xB9 + LE imm32. Upper 32 bits of RCX
        // are zeroed by the implicit zero-extension.
        let program = AsmProgram {
            insts: vec![AsmInst::MovEcxImm32(0x12345678)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0xB9, 0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn add_mem_r13_bl_encodes_to_41_00_5d_00() {
        // add byte [r13+0], bl — REX.B + 0x00 (add r/m8, r8) +
        // ModRM(mod=01, reg=bl=3, rm=r13_low3=5) + disp8(0).
        let program = AsmProgram {
            insts: vec![AsmInst::AddMemR13Bl],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x00, 0x5D, 0x00]);
    }

    #[test]
    fn sub_mem_r13_bl_encodes_to_41_28_5d_00() {
        // sub byte [r13+0], bl — REX.B + 0x28 (sub r/m8, r8) +
        // ModRM(mod=01, reg=bl=3, rm=r13_low3=5) + disp8(0).
        let program = AsmProgram {
            insts: vec![AsmInst::SubMemR13Bl],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x28, 0x5D, 0x00]);
    }

    #[test]
    fn add_mem_r13_bl_disp8_encodes_with_signed_displacement() {
        let program = AsmProgram {
            insts: vec![AsmInst::AddMemR13BlDisp8(5)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x00, 0x5D, 0x05]);
        // Negative displacement
        let program = AsmProgram {
            insts: vec![AsmInst::AddMemR13BlDisp8(-3)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x00, 0x5D, 0xFD]);
    }

    #[test]
    fn sub_mem_r13_bl_disp8_encodes_with_signed_displacement() {
        let program = AsmProgram {
            insts: vec![AsmInst::SubMemR13BlDisp8(7)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x28, 0x5D, 0x07]);
    }

    #[test]
    fn add_mem_r13_al_disp8_encodes_with_signed_displacement() {
        let program = AsmProgram {
            insts: vec![AsmInst::AddMemR13AlDisp8(10)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x41, 0x00, 0x45, 0x0A]);
    }

    #[test]
    fn rep_stosb_encodes_to_f3_aa() {
        // rep stosb — REP prefix 0xF3, then STOSB opcode 0xAA.
        let program = AsmProgram {
            insts: vec![AsmInst::RepStosb],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0xF3, 0xAA]);
    }

    #[test]
    fn lea_rsi_rbp_minus_4096_uses_disp32() {
        // lea rsi, [rbp - 4096] — disp -4096 does not fit i8, so the
        // encoder must select mod=10 (disp32).  REX.W=0x48, opcode=0x8D,
        // ModRM(mod=10, reg=rsi=6, rm=rbp=5) = 0xB5, disp32 = -4096 LE
        // = 0x00 0xF0 0xFF 0xFF.
        let program = AsmProgram {
            insts: vec![AsmInst::LeaRegMem(Reg64::Rsi, Reg64::Rbp, -4096)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text, vec![0x48, 0x8D, 0xB5, 0x00, 0xF0, 0xFF, 0xFF]);
    }

    #[test]
    fn align16_emits_correct_nop_padding() {
        // 1-byte instruction (ret) + Align16 should pad to offset 16.
        let program = AsmProgram {
            insts: vec![AsmInst::Ret, AsmInst::Align16, AsmInst::Ret],
        };
        let encoded = encode_program(&program);
        // Ret = 1 byte at offset 0. Align16 pads 15 bytes (offsets 1..16).
        // Second Ret at offset 16.
        assert_eq!(encoded.text.len(), 17);
        assert_eq!(encoded.text[0], 0xC3); // ret
        assert_eq!(encoded.text[16], 0xC3); // ret at aligned offset
    }

    #[test]
    fn align16_already_aligned_emits_nothing() {
        // 16 bytes of NOPs (two 8-byte NOPs) + Align16 should emit 0 padding.
        let program = AsmProgram {
            insts: vec![
                AsmInst::RawBytes(vec![0x90; 16]),
                AsmInst::Align16,
                AsmInst::Ret,
            ],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 17); // 16 + 0 padding + 1 ret
        assert_eq!(encoded.text[16], 0xC3);
    }
}
