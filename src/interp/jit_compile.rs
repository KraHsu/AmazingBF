//! Hot-loop JIT compilation for the tiered interpreter (F1b-P1).
//!
//! When the interpreter's loop profiler identifies a hot loop (trip count
//! exceeding the configured threshold), this module compiles the loop body
//! from `InterpOp` bytecode to x86_64 machine code via the existing
//! LIR → asm → encode pipeline.
//!
//! The compiled function has the same SysV ABI signature as H2's
//! `compile_lir_to_jit_asm`: `fn(tape_base, data_ptr, tape_end) -> i32`.
//! The caller is responsible for bridging the interpreter's split `Tape`
//! to/from the flat JIT buffer.

#[cfg(target_os = "linux")]
use crate::backend::codegen::compile_lir_to_jit_loop_asm;
#[cfg(target_os = "linux")]
use crate::backend::x86_64::encode::encode_program;
#[cfg(target_os = "linux")]
use crate::backend::x86_64::relax::relax_jumps;
#[cfg(target_os = "linux")]
use crate::interp::bytecode::InterpOp;
#[cfg(target_os = "linux")]
use crate::ir::lir::{LabelGen, LirInst, LirProgram};

/// Lower a loop body (the `InterpOp` slice between `LoopStart` and
/// `LoopEnd`, exclusive) into LIR wrapped in a loop structure.
#[cfg(target_os = "linux")]
fn lower_loop_body_to_lir(body: &[InterpOp]) -> LirProgram {
    let mut labels = LabelGen::new();
    let loop_label = labels.fresh();
    let end_label = labels.fresh();

    let mut insts = Vec::with_capacity(body.len() + 4);
    insts.push(LirInst::Label(loop_label));
    insts.push(LirInst::JumpIfZero(end_label));

    lower_ops_to_lir(body, &mut insts, &mut labels);

    insts.push(LirInst::JumpIfNonZero(loop_label));
    insts.push(LirInst::Label(end_label));
    LirProgram { insts }
}

#[cfg(target_os = "linux")]
fn lower_ops_to_lir(ops: &[InterpOp], lir: &mut Vec<LirInst>, labels: &mut LabelGen) {
    let mut i = 0;
    while i < ops.len() {
        match &ops[i] {
            InterpOp::Move(d) => {
                lir.push(LirInst::PtrAdd(*d as isize));
            }
            InterpOp::Add(k) => {
                lir.push(LirInst::CellAdd(*k));
            }
            InterpOp::MoveAdd { d, k } => {
                lir.push(LirInst::PtrAdd(*d as isize));
                lir.push(LirInst::CellAdd(*k));
            }
            InterpOp::ZeroMove(d) => {
                lir.push(LirInst::CellSet(0));
                lir.push(LirInst::PtrAdd(*d as isize));
            }
            InterpOp::PutByte => lir.push(LirInst::PutByte),
            InterpOp::GetByte => lir.push(LirInst::GetByte),
            InterpOp::Zero => lir.push(LirInst::CellSet(0)),
            InterpOp::LinearMul(plan) => {
                let factors: Vec<(isize, i32)> = plan
                    .factors
                    .iter()
                    .map(|(off, f)| (*off as isize, *f as i32))
                    .collect();
                lir.push(LirInst::LinearMul(factors));
            }
            InterpOp::LinearMulWithSets(plan) => {
                let factors: Vec<(isize, i32)> = plan
                    .factors
                    .iter()
                    .map(|(off, f)| (*off as isize, *f as i32))
                    .collect();
                let sets: Vec<isize> = plan.sets.iter().map(|o| *o as isize).collect();
                lir.push(LirInst::LinearMulWithSets { factors, sets });
            }
            InterpOp::Scan(dir) => {
                lir.push(LirInst::Scan(*dir as isize));
            }
            InterpOp::LoopStart { .. } => {
                let nl = labels.fresh();
                let nel = labels.fresh();
                lir.push(LirInst::Label(nl));
                lir.push(LirInst::JumpIfZero(nel));

                let mut depth = 1u32;
                let mut j = i + 1;
                while j < ops.len() {
                    match &ops[j] {
                        InterpOp::LoopStart { .. } => depth += 1,
                        InterpOp::LoopEnd { .. } => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }

                let inner_body = &ops[i + 1..j];
                lower_ops_to_lir(inner_body, lir, labels);

                lir.push(LirInst::JumpIfNonZero(nl));
                lir.push(LirInst::Label(nel));

                i = j;
            }
            InterpOp::LoopEnd { .. } => {}
        }
        i += 1;
    }
}

/// Compile a hot loop body to a JIT buffer ready for execution.
///
/// `body` is the slice of `InterpOp`s between (exclusive) the `LoopStart`
/// and `LoopEnd` instructions. Returns `None` if compilation fails.
#[cfg(target_os = "linux")]
pub(crate) fn compile_hot_loop(body: &[InterpOp]) -> Option<amazingbf_jit::JitBuffer> {
    let lir = lower_loop_body_to_lir(body);
    let asm = relax_jumps(compile_lir_to_jit_loop_asm(&lir));
    let encoded = encode_program(&asm);
    amazingbf_jit::JitBuffer::new(&encoded.text).ok()
}

/// Decide whether a loop body is JIT-eligible under the F1b-P2 v1 contract,
/// returning the per-iteration touched range relative to the loop entry
/// pointer if so.
///
/// v1 contract (deliberately strict):
/// - **balanced**: cumulative pointer delta is zero, so the JIT-emitted loop
///   exits with `data_ptr` at its entry position; the interpreter can reuse
///   the entry `data_ptr_offset` when restoring the tape.
/// - **bounded reach**: every cell touched (Move/MoveAdd/ZeroMove plus the
///   factor / set offsets of `LinearMul*`) lies in a finite `[min, max]`
///   window relative to entry; this lets the caller pre-grow the tape so
///   the JIT-side `ensure_tape` never fires.
/// - **no top-level nested loops, scans, or I/O**: `LoopStart` / `LoopEnd`
///   inside the body, `Scan`, `PutByte`, `GetByte` all reject the loop.
///   Note that `LinearMul` / `LinearMulWithSets` are *not* nested loops —
///   they are single ops produced by B7-α and are accepted.
///
/// Returns `Some((min_off, max_off))` on success, `None` otherwise.
#[cfg(target_os = "linux")]
pub(crate) fn analyse_eligibility(body: &[InterpOp]) -> Option<(i32, i32)> {
    let mut ptr: i32 = 0;
    let mut min_off: i32 = 0;
    let mut max_off: i32 = 0;
    // The loop's head cell sits at offset 0, and the JIT'd code reads it on
    // every iteration via the leading `cmp byte [r13], 0`. Touch the bounds
    // explicitly so callers always pre-grow the tape to cover offset 0.
    record(&mut min_off, &mut max_off, 0);

    for op in body {
        match op {
            InterpOp::Move(d) => {
                ptr = ptr.checked_add(*d)?;
                record(&mut min_off, &mut max_off, ptr);
            }
            InterpOp::MoveAdd { d, k: _ } => {
                ptr = ptr.checked_add(*d)?;
                record(&mut min_off, &mut max_off, ptr);
            }
            InterpOp::ZeroMove(d) => {
                // ZeroMove writes the cell at the current ptr first, then moves.
                record(&mut min_off, &mut max_off, ptr);
                ptr = ptr.checked_add(*d)?;
                record(&mut min_off, &mut max_off, ptr);
            }
            InterpOp::Add(_) | InterpOp::Zero => {
                record(&mut min_off, &mut max_off, ptr);
            }
            InterpOp::LinearMul(plan) => {
                // The head cell (at ptr) is read and zeroed.
                record(&mut min_off, &mut max_off, ptr);
                for (off, _) in plan.factors.iter() {
                    let abs = ptr.checked_add(*off)?;
                    record(&mut min_off, &mut max_off, abs);
                }
            }
            InterpOp::LinearMulWithSets(plan) => {
                record(&mut min_off, &mut max_off, ptr);
                for (off, _) in plan.factors.iter() {
                    let abs = ptr.checked_add(*off)?;
                    record(&mut min_off, &mut max_off, abs);
                }
                for off in plan.sets.iter() {
                    let abs = ptr.checked_add(*off)?;
                    record(&mut min_off, &mut max_off, abs);
                }
            }
            // Reject IO, Scan (unbounded), and nested loops (v1 keeps the
            // analysis strictly straight-line).
            InterpOp::PutByte
            | InterpOp::GetByte
            | InterpOp::Scan(_)
            | InterpOp::LoopStart { .. }
            | InterpOp::LoopEnd { .. } => return None,
        }
    }

    if ptr != 0 {
        return None;
    }
    Some((min_off, max_off))
}

#[cfg(target_os = "linux")]
#[inline]
fn record(min: &mut i32, max: &mut i32, off: i32) {
    if off < *min {
        *min = off;
    }
    if off > *max {
        *max = off;
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::ir::lir::LabelId;
    use std::sync::Arc;

    fn lid(n: u32) -> LabelId {
        LabelId(n)
    }

    #[test]
    fn lower_simple_body_produces_valid_lir() {
        let body = vec![InterpOp::Add(-1)];
        let lir = lower_loop_body_to_lir(&body);
        assert_eq!(lir.insts[0], LirInst::Label(lid(0)));
        assert_eq!(lir.insts[1], LirInst::JumpIfZero(lid(1)));
        assert_eq!(lir.insts[2], LirInst::CellAdd(-1));
        assert_eq!(lir.insts[3], LirInst::JumpIfNonZero(lid(0)));
        assert_eq!(lir.insts[4], LirInst::Label(lid(1)));
    }

    #[test]
    fn compile_hot_loop_produces_jit_buffer() {
        let body = vec![InterpOp::Add(-1)];
        let buf = compile_hot_loop(&body);
        assert!(buf.is_some(), "compile_hot_loop should succeed for [-]");
    }

    #[test]
    fn compile_move_add_body() {
        let body = vec![InterpOp::MoveAdd { d: 1, k: 1 }, InterpOp::Add(-1)];
        let buf = compile_hot_loop(&body);
        assert!(buf.is_some());
    }

    #[test]
    fn compile_linear_mul_body() {
        use crate::interp::bytecode::LinearMulPlan;
        let plan = Arc::new(LinearMulPlan {
            factors: vec![(1, 1)].into_boxed_slice(),
        });
        let body = vec![InterpOp::LinearMul(plan)];
        let buf = compile_hot_loop(&body);
        assert!(buf.is_some());
    }

    #[test]
    fn eligible_hanoi_pattern() {
        // hanoi.b's hottest loop body: `Move(1) Zero Move(-1) Add(-1)`.
        let body = vec![
            InterpOp::Move(1),
            InterpOp::Zero,
            InterpOp::Move(-1),
            InterpOp::Add(-1),
        ];
        assert_eq!(analyse_eligibility(&body), Some((0, 1)));
    }

    #[test]
    fn eligible_simple_decrement() {
        // [-]: just `Add(-1)`.
        let body = vec![InterpOp::Add(-1)];
        assert_eq!(analyse_eligibility(&body), Some((0, 0)));
    }

    #[test]
    fn eligible_linear_mul_widens_reach() {
        let plan = Arc::new(crate::interp::bytecode::LinearMulPlan {
            factors: vec![(3, 2), (-2, 1)].into_boxed_slice(),
        });
        let body = vec![InterpOp::LinearMul(plan)];
        assert_eq!(analyse_eligibility(&body), Some((-2, 3)));
    }

    #[test]
    fn eligible_linear_mul_with_sets() {
        let plan = Arc::new(crate::interp::bytecode::LinearMulWithSetsPlan {
            factors: vec![(1, 1)].into_boxed_slice(),
            sets: vec![5].into_boxed_slice(),
        });
        let body = vec![InterpOp::LinearMulWithSets(plan)];
        assert_eq!(analyse_eligibility(&body), Some((0, 5)));
    }

    #[test]
    fn ineligible_unbalanced_pointer() {
        let body = vec![InterpOp::Move(1), InterpOp::Add(-1)];
        assert_eq!(analyse_eligibility(&body), None);
    }

    #[test]
    fn ineligible_contains_scan() {
        let body = vec![InterpOp::Scan(1)];
        assert_eq!(analyse_eligibility(&body), None);
    }

    #[test]
    fn ineligible_contains_io() {
        let put = vec![InterpOp::PutByte, InterpOp::Add(-1)];
        let get = vec![InterpOp::GetByte, InterpOp::Add(-1)];
        assert_eq!(analyse_eligibility(&put), None);
        assert_eq!(analyse_eligibility(&get), None);
    }

    #[test]
    fn ineligible_contains_nested_loop() {
        let body = vec![
            InterpOp::Add(-1),
            InterpOp::LoopStart { end_pc: 99 },
            InterpOp::Add(-1),
            InterpOp::LoopEnd { start_pc: 99 },
        ];
        assert_eq!(analyse_eligibility(&body), None);
    }

    #[test]
    fn empty_body_is_balanced_at_origin() {
        // An empty body counts as balanced (ptr stays 0); reach is just the
        // head cell at offset 0.
        assert_eq!(analyse_eligibility(&[]), Some((0, 0)));
    }

    #[test]
    fn lower_nested_loop_body() {
        let body = vec![
            InterpOp::Move(1),
            InterpOp::LoopStart { end_pc: 999 },
            InterpOp::Add(-1),
            InterpOp::LoopEnd { start_pc: 999 },
            InterpOp::Move(-1),
            InterpOp::Add(-1),
        ];
        let lir = lower_loop_body_to_lir(&body);
        assert_eq!(lir.insts[0], LirInst::Label(lid(0)));
        assert_eq!(lir.insts[1], LirInst::JumpIfZero(lid(1)));
        assert_eq!(lir.insts[2], LirInst::PtrAdd(1));
        assert_eq!(lir.insts[3], LirInst::Label(lid(2)));
        assert_eq!(lir.insts[4], LirInst::JumpIfZero(lid(3)));
        assert_eq!(lir.insts[5], LirInst::CellAdd(-1));
        assert_eq!(lir.insts[6], LirInst::JumpIfNonZero(lid(2)));
        assert_eq!(lir.insts[7], LirInst::Label(lid(3)));
    }
}
