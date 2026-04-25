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

use std::collections::HashMap;

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
        normalize_labels(slice_between_markers(&elf)),
        normalize_labels(slice_between_markers(&pe)),
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

/// Rewrite every `AsmLabel(_)` so the two backends' independently-allocated
/// label spaces collapse to a canonical [0, N) indexing in encounter order.
/// The structural sequence is preserved — only the underlying integers change.
/// This isolates the parity comparison from each backend's internal label
/// allocator (Linux decrements from `u32::MAX - 4`, Windows from `0xFFFE_FFFF`).
fn normalize_labels(insts: &[AsmInst]) -> Vec<AsmInst> {
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut next: u32 = 0;
    let mut canon = |label: AsmLabel| -> AsmLabel {
        let id = *remap.entry(label.0).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
        AsmLabel(id)
    };

    insts
        .iter()
        .map(|inst| match inst {
            AsmInst::Label(l) => AsmInst::Label(canon(*l)),
            AsmInst::Jmp(l) => AsmInst::Jmp(canon(*l)),
            AsmInst::JmpShort(l) => AsmInst::JmpShort(canon(*l)),
            AsmInst::Jz(l) => AsmInst::Jz(canon(*l)),
            AsmInst::JzShort(l) => AsmInst::JzShort(canon(*l)),
            AsmInst::Jnz(l) => AsmInst::Jnz(canon(*l)),
            AsmInst::JnzShort(l) => AsmInst::JnzShort(canon(*l)),
            AsmInst::Jb(l) => AsmInst::Jb(canon(*l)),
            AsmInst::Jae(l) => AsmInst::Jae(canon(*l)),
            AsmInst::Jl(l) => AsmInst::Jl(canon(*l)),
            AsmInst::Call(l) => AsmInst::Call(canon(*l)),
            AsmInst::CallMemLabel(l) => AsmInst::CallMemLabel(canon(*l)),
            AsmInst::LeaRegLabel(reg, l) => AsmInst::LeaRegLabel(*reg, canon(*l)),
            other => other.clone(),
        })
        .collect()
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

// ---- D2 SIMD: ScanWithHint must lower identically on both backends ----

#[test]
fn scan_with_hint_positive_parity() {
    assert_parity(
        "ScanWithHint(+1, 16)",
        vec![LirInst::ScanWithHint {
            dir: 1,
            hint_bytes: 16,
        }],
    );
}

#[test]
fn scan_with_hint_negative_parity() {
    assert_parity(
        "ScanWithHint(-1, 8)",
        vec![LirInst::ScanWithHint {
            dir: -1,
            hint_bytes: 8,
        }],
    );
}

// ---- LinearMul ±1 fast path ----
//
// The body of LinearMul is *not* ABI-neutral: the Windows backend wraps it
// with an extra `AddRegImm32(Rsp, ±8)` pair to keep RSP 16-byte aligned at
// the inner `call ensure_tape` site (Win64 ABI; Linux uses bare push/pop
// because syscall has no alignment requirement — see the comment in
// `windows.rs::compile_lir_to_windows_program` LinearMul arm). The per-
// column ±1 fast-path symmetry itself is checked by parallel unit tests
// in `codegen.rs::tests` and `windows.rs::tests` (AddMemR13Bl /
// SubMemR13Bl emission, no-imul shape) — those assert byte-equal column
// bodies without conflating with the alignment cushion.

// ---- D2 SIMD: ZeroRun ----

#[test]
fn zero_run_small_count_parity() {
    // Below the SIMD threshold: scalar store form.
    assert_parity(
        "ZeroRun(start=0, count=4)",
        vec![LirInst::ZeroRun { start: 0, count: 4 }],
    );
    assert_parity(
        "ZeroRun(start=-2, count=8)",
        vec![LirInst::ZeroRun {
            start: -2,
            count: 8,
        }],
    );
}

#[test]
fn zero_run_large_count_parity() {
    // At/above the threshold: rep stosb form.
    assert_parity(
        "ZeroRun(start=0, count=16)",
        vec![LirInst::ZeroRun {
            start: 0,
            count: 16,
        }],
    );
    assert_parity(
        "ZeroRun(start=-3, count=64)",
        vec![LirInst::ZeroRun {
            start: -3,
            count: 64,
        }],
    );
}

#[test]
fn scan_with_hint_zero_hint_parity() {
    // hint=0 must collapse to the slow_top body alone — both backends
    // must agree on that minimal shape (including the same set of
    // emit_ptr_add_out internals).
    assert_parity(
        "ScanWithHint(+1, 0)",
        vec![LirInst::ScanWithHint {
            dir: 1,
            hint_bytes: 0,
        }],
    );
}
