//! LIR-level peephole pass: folds adjacent pointer / cell updates produced by
//! mechanical HIR→LIR lowering.
//!
//! The lowering in [`crate::ir::lower`] keeps a 1:1 correspondence with HIR
//! variants, so sequences like `PtrAdd(1); PtrAdd(1)` or
//! `CellSet(0); CellAdd(1)` fall out naturally. This single forward pass
//! collapses those adjacencies without reordering or crossing any of the
//! "barrier" variants (`Label`, `JumpIfZero`, `JumpIfNonZero`, `Scan`,
//! `LinearMul`, `PutByte`, `GetByte`) — a non-matching variant simply fails
//! every fold pattern and becomes a natural boundary.
//!
//! The pass is a pure value-in / value-out transformation over `LirProgram`,
//! mirroring the style of [`crate::ir::optimize::optimize_o0`].
//!
//! Folding rules:
//! 1. `PtrAdd(a); PtrAdd(b)` → `PtrAdd(a+b)` (removed if the sum is 0)
//! 2. `CellAdd(a); CellAdd(b)` → `CellAdd(a+b)` (removed if the sum is 0 mod 256)
//! 3. `CellSet(v); CellAdd(k)` → `CellSet((v+k) mod 256)`
//! 4. `CellSet(_); CellSet(b)` → `CellSet(b)` (first write is dead)
//! 5. `CellAdd(_); CellSet(v)` → `CellSet(v)` (first update is dead)
//!
//! The displacement-form writes produced by [`crate::ir::lir_postpone`] share
//! the same rules **when both touch the same offset**; cross-offset pairs
//! never fold (they target independent cells):
//! 6. `CellAddAt(off, a); CellAddAt(off, b)` → `CellAddAt(off, a+b)`
//! 7. `CellSetAt(off, v); CellAddAt(off, k)` → `CellSetAt(off, (v+k) mod 256)`
//! 8. `CellSetAt(off, _); CellSetAt(off, b)` → `CellSetAt(off, b)`
//! 9. `CellAddAt(off, _); CellSetAt(off, v)` → `CellSetAt(off, v)`

use crate::ir::lir::{LirInst, LirProgram};

/// Fold adjacent pointer / cell updates in `program`. See the module-level
/// doc for the full rule list.
pub(crate) fn optimize_lir(program: LirProgram) -> LirProgram {
    let mut out: Vec<LirInst> = Vec::with_capacity(program.insts.len());
    for inst in program.insts {
        fold_push(&mut out, inst);
    }
    LirProgram { insts: out }
}

/// Result of attempting to fold `incoming` into the tail of `out`.
enum FoldOutcome {
    /// No fold applies; push `incoming` unchanged.
    Keep,
    /// Replace the tail with this instruction.
    Replace(LirInst),
    /// Drop the tail entirely; the pair cancelled out.
    Drop,
}

fn fold_push(out: &mut Vec<LirInst>, incoming: LirInst) {
    let outcome = match (out.last(), &incoming) {
        (Some(LirInst::PtrAdd(a)), LirInst::PtrAdd(b)) => {
            let sum = a.wrapping_add(*b);
            if sum == 0 {
                FoldOutcome::Drop
            } else {
                FoldOutcome::Replace(LirInst::PtrAdd(sum))
            }
        }
        (Some(LirInst::CellAdd(a)), LirInst::CellAdd(b)) => {
            let sum = a.wrapping_add(*b);
            if sum.rem_euclid(256) == 0 {
                FoldOutcome::Drop
            } else {
                FoldOutcome::Replace(LirInst::CellAdd(sum))
            }
        }
        (Some(LirInst::CellSet(v)), LirInst::CellAdd(k)) => {
            let folded = ((*v as i32) + *k).rem_euclid(256) as u8;
            FoldOutcome::Replace(LirInst::CellSet(folded))
        }
        (Some(LirInst::CellSet(_)), LirInst::CellSet(b)) => {
            FoldOutcome::Replace(LirInst::CellSet(*b))
        }
        (Some(LirInst::CellAdd(_)), LirInst::CellSet(v)) => {
            FoldOutcome::Replace(LirInst::CellSet(*v))
        }
        (
            Some(LirInst::CellAddAt { off: o1, delta: a }),
            LirInst::CellAddAt { off: o2, delta: b },
        ) if o1 == o2 => {
            let sum = a.wrapping_add(*b);
            if sum.rem_euclid(256) == 0 {
                FoldOutcome::Drop
            } else {
                FoldOutcome::Replace(LirInst::CellAddAt {
                    off: *o1,
                    delta: sum,
                })
            }
        }
        (
            Some(LirInst::CellSetAt { off: o1, val: v }),
            LirInst::CellAddAt { off: o2, delta: k },
        ) if o1 == o2 => {
            let folded = ((*v as i32) + *k).rem_euclid(256) as u8;
            FoldOutcome::Replace(LirInst::CellSetAt {
                off: *o1,
                val: folded,
            })
        }
        (Some(LirInst::CellSetAt { off: o1, .. }), LirInst::CellSetAt { off: o2, val: b })
            if o1 == o2 =>
        {
            FoldOutcome::Replace(LirInst::CellSetAt { off: *o1, val: *b })
        }
        (Some(LirInst::CellAddAt { off: o1, .. }), LirInst::CellSetAt { off: o2, val: v })
            if o1 == o2 =>
        {
            FoldOutcome::Replace(LirInst::CellSetAt { off: *o1, val: *v })
        }
        _ => FoldOutcome::Keep,
    };

    match outcome {
        FoldOutcome::Keep => out.push(incoming),
        FoldOutcome::Replace(new_inst) => {
            *out.last_mut().expect("fold implies non-empty tail") = new_inst;
        }
        FoldOutcome::Drop => {
            out.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lir::{LabelGen, LirInst};

    fn run(insts: Vec<LirInst>) -> Vec<LirInst> {
        optimize_lir(LirProgram { insts }).insts
    }

    #[test]
    fn fold_ptr_add_adjacent() {
        let out = run(vec![LirInst::PtrAdd(3), LirInst::PtrAdd(4)]);
        assert_eq!(out, vec![LirInst::PtrAdd(7)]);
    }

    #[test]
    fn fold_ptr_add_cancels_to_zero() {
        let out = run(vec![LirInst::PtrAdd(5), LirInst::PtrAdd(-5)]);
        assert!(out.is_empty());
    }

    #[test]
    fn fold_cell_add_adjacent() {
        let out = run(vec![LirInst::CellAdd(2), LirInst::CellAdd(3)]);
        assert_eq!(out, vec![LirInst::CellAdd(5)]);
    }

    #[test]
    fn fold_cell_add_cancels_mod_256() {
        let out = run(vec![LirInst::CellAdd(100), LirInst::CellAdd(156)]);
        assert!(out.is_empty());
    }

    #[test]
    fn fold_cell_add_cancels_negative_wraparound() {
        let out = run(vec![LirInst::CellAdd(1), LirInst::CellAdd(-1)]);
        assert!(out.is_empty());
    }

    #[test]
    fn fold_cell_set_then_cell_add() {
        let out = run(vec![LirInst::CellSet(10), LirInst::CellAdd(5)]);
        assert_eq!(out, vec![LirInst::CellSet(15)]);
    }

    #[test]
    fn fold_cell_set_then_cell_add_wraps() {
        let out = run(vec![LirInst::CellSet(250), LirInst::CellAdd(10)]);
        assert_eq!(out, vec![LirInst::CellSet(4)]);
    }

    #[test]
    fn fold_cell_set_then_cell_add_negative() {
        let out = run(vec![LirInst::CellSet(5), LirInst::CellAdd(-10)]);
        assert_eq!(out, vec![LirInst::CellSet(251)]);
    }

    #[test]
    fn fold_cell_set_then_cell_set() {
        let out = run(vec![LirInst::CellSet(10), LirInst::CellSet(20)]);
        assert_eq!(out, vec![LirInst::CellSet(20)]);
    }

    #[test]
    fn fold_cell_add_then_cell_set() {
        let out = run(vec![LirInst::CellAdd(7), LirInst::CellSet(42)]);
        assert_eq!(out, vec![LirInst::CellSet(42)]);
    }

    #[test]
    fn fold_chain_of_ptr_adds() {
        let out = run(vec![
            LirInst::PtrAdd(1),
            LirInst::PtrAdd(2),
            LirInst::PtrAdd(3),
            LirInst::PtrAdd(-4),
        ]);
        assert_eq!(out, vec![LirInst::PtrAdd(2)]);
    }

    #[test]
    fn fold_does_not_cross_label() {
        let mut labels = LabelGen::new();
        let lbl = labels.fresh();
        let out = run(vec![
            LirInst::PtrAdd(1),
            LirInst::Label(lbl),
            LirInst::PtrAdd(2),
        ]);
        assert_eq!(
            out,
            vec![LirInst::PtrAdd(1), LirInst::Label(lbl), LirInst::PtrAdd(2)]
        );
    }

    #[test]
    fn fold_does_not_cross_jump_if_zero() {
        let mut labels = LabelGen::new();
        let lbl = labels.fresh();
        let out = run(vec![
            LirInst::CellAdd(1),
            LirInst::JumpIfZero(lbl),
            LirInst::CellAdd(2),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::CellAdd(1),
                LirInst::JumpIfZero(lbl),
                LirInst::CellAdd(2),
            ]
        );
    }

    #[test]
    fn fold_does_not_cross_put_byte() {
        let out = run(vec![
            LirInst::CellAdd(1),
            LirInst::PutByte,
            LirInst::CellAdd(2),
        ]);
        assert_eq!(
            out,
            vec![LirInst::CellAdd(1), LirInst::PutByte, LirInst::CellAdd(2)]
        );
    }

    #[test]
    fn fold_does_not_cross_get_byte() {
        let out = run(vec![
            LirInst::CellSet(7),
            LirInst::GetByte,
            LirInst::CellSet(9),
        ]);
        assert_eq!(
            out,
            vec![LirInst::CellSet(7), LirInst::GetByte, LirInst::CellSet(9)]
        );
    }

    #[test]
    fn fold_does_not_cross_scan() {
        let out = run(vec![
            LirInst::PtrAdd(1),
            LirInst::Scan(1),
            LirInst::PtrAdd(2),
        ]);
        assert_eq!(
            out,
            vec![LirInst::PtrAdd(1), LirInst::Scan(1), LirInst::PtrAdd(2)]
        );
    }

    #[test]
    fn empty_program_stays_empty() {
        let out = run(vec![]);
        assert!(out.is_empty());
    }

    #[test]
    fn singleton_program_passes_through() {
        let out = run(vec![LirInst::CellAdd(3)]);
        assert_eq!(out, vec![LirInst::CellAdd(3)]);
    }

    #[test]
    fn fold_cell_add_at_same_offset() {
        let out = run(vec![
            LirInst::CellAddAt { off: 3, delta: 1 },
            LirInst::CellAddAt { off: 3, delta: 2 },
        ]);
        assert_eq!(out, vec![LirInst::CellAddAt { off: 3, delta: 3 }]);
    }

    #[test]
    fn fold_cell_add_at_same_offset_cancels_mod_256() {
        let out = run(vec![
            LirInst::CellAddAt { off: 3, delta: 100 },
            LirInst::CellAddAt { off: 3, delta: 156 },
        ]);
        assert!(out.is_empty());
    }

    #[test]
    fn fold_cell_add_at_different_offset_keeps_both() {
        let out = run(vec![
            LirInst::CellAddAt { off: 3, delta: 1 },
            LirInst::CellAddAt { off: 4, delta: 1 },
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::CellAddAt { off: 3, delta: 1 },
                LirInst::CellAddAt { off: 4, delta: 1 },
            ]
        );
    }

    #[test]
    fn fold_cell_set_at_then_cell_add_at_same_offset() {
        let out = run(vec![
            LirInst::CellSetAt { off: 3, val: 5 },
            LirInst::CellAddAt { off: 3, delta: 10 },
        ]);
        assert_eq!(out, vec![LirInst::CellSetAt { off: 3, val: 15 }]);
    }

    #[test]
    fn fold_cell_set_at_then_cell_set_at_same_offset_keeps_last() {
        let out = run(vec![
            LirInst::CellSetAt { off: 3, val: 5 },
            LirInst::CellSetAt { off: 3, val: 9 },
        ]);
        assert_eq!(out, vec![LirInst::CellSetAt { off: 3, val: 9 }]);
    }

    #[test]
    fn fold_cell_add_at_then_cell_set_at_same_offset_keeps_set() {
        let out = run(vec![
            LirInst::CellAddAt { off: 3, delta: 7 },
            LirInst::CellSetAt { off: 3, val: 9 },
        ]);
        assert_eq!(out, vec![LirInst::CellSetAt { off: 3, val: 9 }]);
    }

    #[test]
    fn fold_cell_set_at_different_offset_keeps_both() {
        let out = run(vec![
            LirInst::CellSetAt { off: 3, val: 5 },
            LirInst::CellSetAt { off: 4, val: 9 },
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::CellSetAt { off: 3, val: 5 },
                LirInst::CellSetAt { off: 4, val: 9 },
            ]
        );
    }
}
