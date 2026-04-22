//! HIR → [`InterpProgram`] lowering.
//!
//! Flattens nested [`HirInst::Loop`] bodies into a single instruction stream
//! with absolute-PC jumps on `LoopStart` / `LoopEnd`, and performs the two
//! local fusions the dispatch loop benefits from most:
//!
//! - `Move(d); Add(k)` → [`InterpOp::MoveAdd`]
//! - `Zero;    Move(d)` → [`InterpOp::ZeroMove`]
//!
//! Jump targets are back-patched in a single sweep: `LoopStart` is emitted
//! with a placeholder, its index pushed onto a scratch stack, and the
//! matching `LoopEnd` writes both endpoints once it knows its own pc.
//!
//! The pass is deterministic: identical HIR always produces identical
//! `InterpProgram`, which keeps the interpreter amenable to golden tests.

use std::sync::Arc;

use crate::interp::bytecode::{InterpOp, InterpProgram, LinearMulPlan};
use crate::ir::hir::{HirInst, HirProgram};

/// Lower a [`HirProgram`] to a flat [`InterpProgram`] with resolved jump
/// targets and fused `MoveAdd` / `ZeroMove` superinstructions.
pub(crate) fn lower_hir_to_bytecode(program: &HirProgram) -> InterpProgram {
    let mut ops: Vec<InterpOp> = Vec::with_capacity(program.insts.len());
    let mut loop_stack: Vec<u32> = Vec::new();
    lower_block(&program.insts, &mut ops, &mut loop_stack);
    debug_assert!(
        loop_stack.is_empty(),
        "HIR loop stack must be balanced after lowering"
    );
    InterpProgram { ops }
}

/// Lower a single HIR block (top-level or inside a `Loop`) by streaming ops
/// into `ops` and pushing any `LoopStart` indices onto `loop_stack` for the
/// matching `LoopEnd` to resolve.
fn lower_block(insts: &[HirInst], ops: &mut Vec<InterpOp>, loop_stack: &mut Vec<u32>) {
    for inst in insts {
        match inst {
            HirInst::Move(d) => {
                let d = clamp_i32("Move delta", *d);
                // Fold `Zero; Move(d)` → `ZeroMove(d)` when the preceding op
                // was a bare `Zero` (either HIR `Zero` or an already-emitted
                // `ZeroMove(0)` shouldn't happen, so only the simple case).
                if matches!(ops.last(), Some(InterpOp::Zero)) {
                    *ops.last_mut().unwrap() = InterpOp::ZeroMove(d);
                } else {
                    ops.push(InterpOp::Move(d));
                }
            }
            HirInst::Add(k) => {
                let k = *k;
                // Fold `Move(d); Add(k)` → `MoveAdd { d, k }`. `Add(0)` is
                // dropped upstream by the HIR fuser, so we don't need to
                // special-case it here — but cheap to defend against.
                if k == 0 {
                    continue;
                }
                if let Some(&InterpOp::Move(d)) = ops.last() {
                    *ops.last_mut().unwrap() = InterpOp::MoveAdd { d, k };
                } else {
                    ops.push(InterpOp::Add(k));
                }
            }
            HirInst::PutByte => ops.push(InterpOp::PutByte),
            HirInst::GetByte => ops.push(InterpOp::GetByte),
            HirInst::Zero => ops.push(InterpOp::Zero),
            HirInst::LinearMul(factors) => {
                let packed: Box<[(i32, i16)]> = factors
                    .iter()
                    .map(|(off, f)| {
                        let off = clamp_i32("LinearMul offset", *off);
                        let f = clamp_i16_mod256("LinearMul factor", *f);
                        (off, f)
                    })
                    .collect();
                ops.push(InterpOp::LinearMul(Arc::new(LinearMulPlan {
                    factors: packed,
                })));
            }
            HirInst::Scan(dir) => {
                let step: i8 = match dir.signum() {
                    1 => 1,
                    -1 => -1,
                    _ => panic!("Scan dir must be non-zero"),
                };
                ops.push(InterpOp::Scan(step));
            }
            HirInst::Loop(body) => {
                let start_pc: u32 = ops
                    .len()
                    .try_into()
                    .expect("bytecode program length exceeds u32::MAX");
                // Placeholder; `end_pc` gets back-patched by the matching
                // `LoopEnd` below.
                ops.push(InterpOp::LoopStart { end_pc: 0 });
                loop_stack.push(start_pc);

                lower_block(body, ops, loop_stack);

                let start_pc = loop_stack
                    .pop()
                    .expect("loop stack is unbalanced: LoopEnd without matching LoopStart");
                let end_pc: u32 = ops
                    .len()
                    .try_into()
                    .expect("bytecode program length exceeds u32::MAX");
                match &mut ops[start_pc as usize] {
                    InterpOp::LoopStart { end_pc: slot } => *slot = end_pc,
                    _ => unreachable!("loop_stack index must point at LoopStart"),
                }
                ops.push(InterpOp::LoopEnd { start_pc });
            }
        }
    }
}

/// Narrow an `isize` move/offset to `i32`, panicking with a descriptive
/// context if the value overflows.
fn clamp_i32(what: &'static str, v: isize) -> i32 {
    i32::try_from(v).unwrap_or_else(|_| panic!("{what} out of i32 range: {v}"))
}

/// `try_linear_loop` already reduces every factor modulo 256, so the range
/// is `-128..=127` by the sign choice in that pass. Guard anyway in case
/// that invariant gets relaxed upstream.
fn clamp_i16_mod256(what: &'static str, v: i32) -> i16 {
    let reduced = v.rem_euclid(256);
    let signed = if reduced <= 127 {
        reduced
    } else {
        reduced - 256
    };
    i16::try_from(signed).unwrap_or_else(|_| panic!("{what} reduction out of i16: {v}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_program_lowers_to_empty_bytecode() {
        let hir = HirProgram { insts: vec![] };
        let bc = lower_hir_to_bytecode(&hir);
        assert!(bc.ops.is_empty());
    }

    #[test]
    fn simple_ops_map_one_to_one() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Add(3),
                HirInst::PutByte,
                HirInst::GetByte,
                HirInst::Zero,
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::Add(3),
                InterpOp::PutByte,
                InterpOp::GetByte,
                InterpOp::Zero,
            ]
        );
    }

    #[test]
    fn move_then_add_fuses_into_move_add() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(2), HirInst::Add(5)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::MoveAdd { d: 2, k: 5 }]);
    }

    #[test]
    fn zero_then_move_fuses_into_zero_move() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::Move(-3)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::ZeroMove(-3)]);
    }

    #[test]
    fn chain_of_move_add_pairs_fuses_pairwise() {
        // `Move(1); Add(2); Move(3); Add(4)` → two `MoveAdd`s.
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(2),
                HirInst::Move(3),
                HirInst::Add(4),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::MoveAdd { d: 1, k: 2 },
                InterpOp::MoveAdd { d: 3, k: 4 },
            ]
        );
    }

    #[test]
    fn add_without_preceding_move_stays_bare() {
        let hir = HirProgram {
            insts: vec![HirInst::Add(4)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Add(4)]);
    }

    #[test]
    fn zero_without_following_move_stays_bare() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::PutByte],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Zero, InterpOp::PutByte]);
    }

    #[test]
    fn loop_resolves_absolute_jump_targets() {
        // [>+<] (without O1): Loop(Move(1), Add(1), Move(-1))
        // Bytecode: LoopStart{end_pc=3} MoveAdd{d=1,k=1} Move(-1) LoopEnd{start_pc=0}
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Move(1),
                HirInst::Add(1),
                HirInst::Move(-1),
            ])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::LoopStart { end_pc: 3 },
                InterpOp::MoveAdd { d: 1, k: 1 },
                InterpOp::Move(-1),
                InterpOp::LoopEnd { start_pc: 0 },
            ]
        );
    }

    #[test]
    fn nested_loops_resolve_jump_targets_independently() {
        // [ [>] ]: Loop(Loop(Scan(1)))
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Loop(vec![HirInst::Scan(1)])])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        // outer: 0 LoopStart → 4 LoopEnd
        // inner: 1 LoopStart → 3 LoopEnd, body at pc=2
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::LoopStart { end_pc: 4 },
                InterpOp::LoopStart { end_pc: 3 },
                InterpOp::Scan(1),
                InterpOp::LoopEnd { start_pc: 1 },
                InterpOp::LoopEnd { start_pc: 0 },
            ]
        );
    }

    #[test]
    fn linear_mul_packs_factors_into_plan() {
        // 171 ≡ -85 (mod 256) and -171 ≡ 85 (mod 256); the packer
        // normalises into the signed range [-128, 127] so the runtime can
        // skip an extra rem_euclid per iteration.
        let hir = HirProgram {
            insts: vec![HirInst::LinearMul(vec![(1, 171), (3, -171)])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        match bc.ops.as_slice() {
            [InterpOp::LinearMul(plan)] => {
                assert_eq!(&*plan.factors, &[(1i32, -85i16), (3, 85)]);
            }
            other => panic!("expected single LinearMul, got {other:?}"),
        }
    }

    #[test]
    fn linear_mul_factor_in_signed_range_preserves_sign() {
        // Already-signed factors like 1 / -1 pass through unchanged.
        let hir = HirProgram {
            insts: vec![HirInst::LinearMul(vec![(2, 1), (4, -1)])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        match bc.ops.as_slice() {
            [InterpOp::LinearMul(plan)] => {
                assert_eq!(&*plan.factors, &[(2i32, 1i16), (4, -1)]);
            }
            other => panic!("expected single LinearMul, got {other:?}"),
        }
    }

    #[test]
    fn scan_converts_dir_to_signum() {
        let hir = HirProgram {
            insts: vec![HirInst::Scan(-1), HirInst::Scan(1)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Scan(-1), InterpOp::Scan(1)]);
    }

    #[test]
    fn add_zero_is_skipped_even_if_upstream_forgets_to_drop_it() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(1), HirInst::Add(0), HirInst::Move(2)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        // `Add(0)` drops, leaving two consecutive Moves — they are not
        // fused by this pass (that's O0's job), so they stay as Moves.
        assert_eq!(bc.ops, vec![InterpOp::Move(1), InterpOp::Move(2)]);
    }
}
