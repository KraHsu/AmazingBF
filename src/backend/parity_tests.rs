//! Cross-backend equivalence test harness.
//!
//! Asserts that for each ABI-independent LIR instruction, the Linux backend
//! (`compile_lir_to_asm`) and the Windows backend (`compile_lir_to_windows_program`)
//! lower it to the same `AsmInst` sequence. The single shared encoder
//! (`backend::x86_64::encode`) then produces byte-for-byte identical machine
//! code from those sequences, so `AsmInst`-level equality is sufficient.
//!
//! What is NOT tested here:
//! - `LirInst::PutByte` / `LirInst::GetByte` — I/O lowering is intentionally
//!   ABI-divergent (Linux uses `syscall`; Windows uses kernel32 IAT calls).
//! - Program prologue / epilogue / init / exit paths — also ABI-divergent.
//!
//! Marker strategy: each fixture is sandwiched between two `Label` markers
//! whose `LabelId`s are picked from `0x7000_0000` upwards — far above any
//! user label produced by HIR / LIR but well below the internal-label
//! reserved range (which both backends decrement from `u32::MAX`). The
//! harness slices the emitted `AsmInst` stream between the matching
//! `AsmInst::Label` instances.

#![cfg(test)]

use crate::backend::asm::{AsmInst, AsmLabel};
use crate::backend::codegen::compile_lir_to_asm;
use crate::backend::x86_64::windows::compile_lir_to_windows_program;
use crate::ir::lir::{LabelId, LirInst, LirProgram};

const PARITY_MARKER_BEGIN: u32 = 0x7000_0000;
const PARITY_MARKER_END: u32 = 0x7000_0001;

fn parity_for_lir(insts: Vec<LirInst>) -> (Vec<AsmInst>, Vec<AsmInst>) {
    let mut wrapped = Vec::with_capacity(insts.len() + 2);
    wrapped.push(LirInst::Label(LabelId(PARITY_MARKER_BEGIN)));
    wrapped.extend(insts);
    wrapped.push(LirInst::Label(LabelId(PARITY_MARKER_END)));
    let lir = LirProgram { insts: wrapped };

    let elf = compile_lir_to_asm(&lir).insts;
    let pe = compile_lir_to_windows_program(&lir).asm.insts;

    (
        slice_between_markers(&elf).to_vec(),
        slice_between_markers(&pe).to_vec(),
    )
}

fn slice_between_markers(insts: &[AsmInst]) -> &[AsmInst] {
    let begin = AsmLabel(PARITY_MARKER_BEGIN);
    let end = AsmLabel(PARITY_MARKER_END);
    let begin_idx = insts
        .iter()
        .position(|i| matches!(i, AsmInst::Label(l) if *l == begin))
        .expect("backend dropped the begin parity marker");
    let end_idx = insts
        .iter()
        .position(|i| matches!(i, AsmInst::Label(l) if *l == end))
        .expect("backend dropped the end parity marker");
    assert!(begin_idx < end_idx, "begin marker must precede end marker");
    &insts[begin_idx + 1..end_idx]
}

#[track_caller]
fn assert_parity(name: &str, insts: Vec<LirInst>) {
    let (elf, pe) = parity_for_lir(insts);
    assert_eq!(
        elf, pe,
        "backend parity violated for `{name}`\n  ELF emits: {elf:#?}\n  PE  emits: {pe:#?}"
    );
}

// ---- CellAdd / CellSet (D1 sync gate) ----

#[test]
fn cell_add_plus_one_parity() {
    assert_parity("CellAdd(1)", vec![LirInst::CellAdd(1)]);
}

#[test]
fn cell_add_minus_one_parity() {
    assert_parity("CellAdd(-1)", vec![LirInst::CellAdd(-1)]);
}

#[test]
fn cell_add_other_values_parity() {
    assert_parity(
        "CellAdd misc",
        vec![
            LirInst::CellAdd(2),
            LirInst::CellAdd(-3),
            LirInst::CellAdd(127),
            LirInst::CellAdd(-128),
        ],
    );
}

#[test]
fn cell_add_modular_zero_is_noop_in_both_backends() {
    // 256 ≡ 0 (mod 256), so neither backend should emit a memory op.
    assert_parity("CellAdd(256)", vec![LirInst::CellAdd(256)]);
    assert_parity("CellAdd(-256)", vec![LirInst::CellAdd(-256)]);
}

#[test]
fn cell_set_parity() {
    assert_parity("CellSet(0)", vec![LirInst::CellSet(0)]);
    assert_parity("CellSet(255)", vec![LirInst::CellSet(255)]);
    assert_parity("CellSet(42)", vec![LirInst::CellSet(42)]);
}

// ---- ABI-independent instruction sanity (sandwich a few non-I/O ops) ----

#[test]
fn cell_add_at_disp8_parity() {
    assert_parity(
        "CellAddAt disp8",
        vec![
            LirInst::CellAddAt { off: 5, delta: 3 },
            LirInst::CellAddAt { off: -7, delta: -2 },
        ],
    );
}

#[test]
fn cell_set_at_disp8_parity() {
    assert_parity(
        "CellSetAt disp8",
        vec![
            LirInst::CellSetAt { off: 4, val: 42 },
            LirInst::CellSetAt { off: -3, val: 0 },
        ],
    );
}
