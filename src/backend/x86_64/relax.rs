//! Branch-relaxation pass: narrows long Jcc / JMP (rel32) forms to their
//! short (rel8) counterparts where the target fits within the signed 8-bit
//! displacement range.
//!
//! ## Why this is safe
//!
//! The codegen in [`crate::backend::codegen`] always emits the long (rel32)
//! forms of Jcc / JMP. This pass is monotonic: it only ever converts long →
//! short, which strictly shrinks the encoded size of the converted
//! instruction. Every non-converted jump's target either stays the same
//! distance away or moves closer — never further — so short choices made in
//! earlier iterations remain in range.
//!
//! Iteration converges in at most O(n) rounds because each pass either
//! narrows at least one jump or terminates.
//!
//! ## Why not fold this into the encoder
//!
//! Encoding happens once the layout is final. Computing the real
//! displacement — and therefore the short-form eligibility — requires
//! knowing every preceding instruction's byte length, which in turn depends
//! on the short-form decision of every preceding jump. That circular
//! dependency is exactly what this iterative relaxation pass resolves.
//!
//! The pass is an `AsmProgram` → `AsmProgram` transformation, independent of
//! ELF / PE packaging; both the Linux and Windows backends run it before
//! encoding.

use std::collections::HashMap;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram};
use crate::backend::x86_64::encode::encode_program_with_inst_map;

/// Byte length of the short Jcc / JMP forms (opcode + rel8).
const SHORT_JUMP_LEN: usize = 2;

/// Relax every long Jcc / JMP whose target fits in rel8, iterating until a
/// fixed point is reached.
///
/// The returned program is byte-for-byte equivalent to the input, except
/// that converted jumps now use the 2-byte short encoding (saving 3 bytes
/// per `Jmp` and 4 bytes per `Jcc`).
pub(crate) fn relax_jumps(mut program: AsmProgram) -> AsmProgram {
    // Monotonic loop: each iteration narrows at least one jump or terminates.
    // At most `jumps.len()` iterations are needed to converge.
    loop {
        let (inst_offsets, label_offsets) = compute_offsets(&program);

        let mut changed = false;
        for (idx, inst) in program.insts.iter_mut().enumerate() {
            let Some((target_label, short_inst)) = try_shorten(inst) else {
                continue;
            };
            let target = *label_offsets
                .get(&target_label)
                .unwrap_or_else(|| panic!("relax: unbound label {:?}", target_label));
            let next_ip = inst_offsets[idx] + SHORT_JUMP_LEN;
            let rel = target as i64 - next_ip as i64;
            if i8::try_from(rel).is_ok() {
                *inst = short_inst;
                changed = true;
            }
        }

        if !changed {
            return program;
        }
    }
}

/// If `inst` is a long Jcc / JMP, return its target label plus the short
/// instruction it would become. Otherwise return `None`.
fn try_shorten(inst: &AsmInst) -> Option<(AsmLabel, AsmInst)> {
    match inst {
        AsmInst::Jz(lbl) => Some((*lbl, AsmInst::JzShort(*lbl))),
        AsmInst::Jnz(lbl) => Some((*lbl, AsmInst::JnzShort(*lbl))),
        AsmInst::Jb(lbl) => Some((*lbl, AsmInst::JbShort(*lbl))),
        AsmInst::Jae(lbl) => Some((*lbl, AsmInst::JaeShort(*lbl))),
        AsmInst::Jl(lbl) => Some((*lbl, AsmInst::JlShort(*lbl))),
        AsmInst::Jge(lbl) => Some((*lbl, AsmInst::JgeShort(*lbl))),
        AsmInst::Jmp(lbl) => Some((*lbl, AsmInst::JmpShort(*lbl))),
        _ => None,
    }
}

/// Encode `program` to recover each instruction's byte offset and every
/// label's resolved offset.
///
/// This delegates to the real encoder rather than hand-maintaining a
/// parallel size table, so the two can never drift apart.
fn compute_offsets(program: &AsmProgram) -> (Vec<usize>, HashMap<AsmLabel, usize>) {
    let (_encoded, inst_map) = encode_program_with_inst_map(program);
    let offsets: Vec<usize> = inst_map.iter().map(|entry| entry.offset).collect();

    let mut label_offsets = HashMap::new();
    for (idx, inst) in program.insts.iter().enumerate() {
        if let AsmInst::Label(label) = inst {
            label_offsets.insert(*label, inst_map[idx].offset);
        }
    }
    (offsets, label_offsets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::asm::{AsmLabel, AsmProgram, Reg64};
    use crate::backend::x86_64::encode::encode_program;

    fn lbl(id: u32) -> AsmLabel {
        AsmLabel(id)
    }

    #[test]
    fn short_jump_chosen_when_target_in_range() {
        let target = lbl(1);
        let program = AsmProgram {
            insts: vec![
                AsmInst::Jz(target),
                AsmInst::Ret,
                AsmInst::Label(target),
                AsmInst::Ret,
            ],
        };
        let relaxed = relax_jumps(program);
        assert!(matches!(relaxed.insts[0], AsmInst::JzShort(_)));
    }

    #[test]
    fn unconditional_jmp_shortens_when_in_range() {
        let target = lbl(2);
        let program = AsmProgram {
            insts: vec![
                AsmInst::Jmp(target),
                AsmInst::Ret,
                AsmInst::Label(target),
                AsmInst::Ret,
            ],
        };
        let relaxed = relax_jumps(program);
        assert!(matches!(relaxed.insts[0], AsmInst::JmpShort(_)));
    }

    #[test]
    fn long_jump_preserved_when_target_out_of_rel8_range() {
        // 200 bytes of padding > 127 → long form required.
        let target = lbl(3);
        let program = AsmProgram {
            insts: vec![
                AsmInst::Jz(target),
                AsmInst::RawBytes(vec![0x90; 200]),
                AsmInst::Label(target),
                AsmInst::Ret,
            ],
        };
        let relaxed = relax_jumps(program);
        assert!(matches!(relaxed.insts[0], AsmInst::Jz(_)));
    }

    #[test]
    fn backward_short_jump_converges() {
        // jmp back over a very short body: the short form definitely fits.
        let head = lbl(4);
        let program = AsmProgram {
            insts: vec![
                AsmInst::Label(head),
                AsmInst::AddRegImm32(Reg64::Rax, 1),
                AsmInst::Jmp(head),
            ],
        };
        let relaxed = relax_jumps(program);
        assert!(matches!(relaxed.insts[2], AsmInst::JmpShort(_)));
    }

    #[test]
    fn chain_of_jumps_reaches_fixed_point() {
        // Several interleaved forward jumps to the same nearby label. The
        // first pass shrinks the closest one; each subsequent pass narrows
        // more because the program shortens. All must end up short.
        let target = lbl(5);
        let mut insts = Vec::new();
        for _ in 0..4 {
            insts.push(AsmInst::Jz(target));
        }
        insts.push(AsmInst::Label(target));
        insts.push(AsmInst::Ret);
        let relaxed = relax_jumps(AsmProgram { insts });
        for inst in &relaxed.insts[..4] {
            assert!(
                matches!(inst, AsmInst::JzShort(_)),
                "expected every jz to shorten: {:?}",
                inst
            );
        }
    }

    #[test]
    fn encoder_emits_two_bytes_for_short_jump() {
        let target = lbl(6);
        let program = AsmProgram {
            insts: vec![
                AsmInst::JzShort(target),
                AsmInst::Label(target),
                AsmInst::Ret,
            ],
        };
        let encoded = encode_program(&program);
        // jz rel8 = 2 bytes; ret = 1 byte.
        assert_eq!(encoded.text.len(), 3);
        assert_eq!(encoded.text[0], 0x74); // opcode jz rel8
        assert_eq!(encoded.text[1], 0x00); // displacement to the immediately-following instruction
    }

    #[test]
    fn short_jump_displacement_is_signed() {
        // Backward short jump: produces a negative rel8.
        let head = lbl(7);
        let program = AsmProgram {
            insts: vec![AsmInst::Label(head), AsmInst::Ret, AsmInst::JmpShort(head)],
        };
        let encoded = encode_program(&program);
        assert_eq!(encoded.text.len(), 3);
        assert_eq!(encoded.text[1], 0xEB); // jmp rel8 opcode
        // target is at offset 0; next_ip = fixup_at(2) + 1 = 3; rel = 0 - 3 = -3 = 0xFD
        assert_eq!(encoded.text[2], 0xFD);
    }
}
