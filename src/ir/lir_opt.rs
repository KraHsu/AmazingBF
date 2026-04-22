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
//!
//! Bounds-check windows produced by B4/C2 share their verified range when
//! adjacent; the combined window is the union, which is still contiguous
//! because the tape itself is contiguous:
//! 10. `PtrAddChecked(d1, lo1, hi1); PtrAddChecked(d2, lo2, hi2)` →
//!     `PtrAddChecked(d1+d2, min(lo1, d1+lo2), max(hi1, d1+hi2))` (merged
//!     probe window). Degenerates to `Drop` if the combined op is a no-op.
//!
//! Contiguous zero writes coalesce into a single `ZeroRun` so D2 can later
//! swap the byte-by-byte stores for `rep stosb`:
//! 11. `CellSet(0); CellSetAt(1, 0)` → `ZeroRun(start=0, count=2)`
//! 12. `CellSetAt(-1, 0); CellSet(0)` → `ZeroRun(start=-1, count=2)`
//! 13. `CellSetAt(k, 0); CellSetAt(k+1, 0)` → `ZeroRun(start=k, count=2)`
//! 14. `ZeroRun(start, count); CellSetAt(start+count, 0)` → extend
//! 15. `ZeroRun(start, count); CellSet(0)` if `start+count == 0` → extend

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
        (
            Some(LirInst::PtrAddChecked {
                delta: d1,
                lo_extent: lo1,
                hi_extent: hi1,
            }),
            LirInst::PtrAddChecked {
                delta: d2,
                lo_extent: lo2,
                hi_extent: hi2,
            },
        ) => {
            // Union the two verified windows, measured against the initial
            // r13 (= r13 at the entry of the first op). The second op's window
            // is offset by d1 because its `r13` is already shifted by that.
            let delta = d1.saturating_add(*d2);
            let lo = (*lo1).min(d1.saturating_add(*lo2));
            let hi = (*hi1).max(d1.saturating_add(*hi2));
            if lo == 0 && hi == 0 && delta == 0 {
                FoldOutcome::Drop
            } else {
                FoldOutcome::Replace(LirInst::PtrAddChecked {
                    delta,
                    lo_extent: lo,
                    hi_extent: hi,
                })
            }
        }
        // Contiguous zero writes coalesce into a ZeroRun so D2 (rep stosb)
        // can later peel them off without having to re-discover the shape.
        (Some(LirInst::CellSet(0)), LirInst::CellSetAt { off: 1, val: 0 }) => {
            FoldOutcome::Replace(LirInst::ZeroRun { start: 0, count: 2 })
        }
        (Some(LirInst::CellSetAt { off: -1, val: 0 }), LirInst::CellSet(0)) => {
            FoldOutcome::Replace(LirInst::ZeroRun {
                start: -1,
                count: 2,
            })
        }
        (
            Some(LirInst::CellSetAt {
                off: prev_off,
                val: 0,
            }),
            LirInst::CellSetAt { off, val: 0 },
        ) if *off == *prev_off + 1 => {
            let start = i32::try_from(*prev_off)
                .expect("CellSetAt off must fit in i32 (lir_postpone DISP32 cap)");
            FoldOutcome::Replace(LirInst::ZeroRun { start, count: 2 })
        }
        (Some(LirInst::ZeroRun { start, count }), LirInst::CellSetAt { off, val: 0 })
            if *off == (*start as isize) + (*count as isize) =>
        {
            FoldOutcome::Replace(LirInst::ZeroRun {
                start: *start,
                count: count + 1,
            })
        }
        (Some(LirInst::ZeroRun { start, count }), LirInst::CellSet(0))
            if (*start as isize) + (*count as isize) == 0 =>
        {
            FoldOutcome::Replace(LirInst::ZeroRun {
                start: *start,
                count: count + 1,
            })
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
    fn fold_adjacent_ptr_add_checked_unions_windows() {
        // Two checked ops advancing by 0 each, windows [-2, 3] and [-1, 4].
        // Combined delta=0, lo=min(-2, 0-1)=-2, hi=max(3, 0+4)=4.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -2,
                hi_extent: 3,
            },
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -1,
                hi_extent: 4,
            },
        ]);
        assert_eq!(
            out,
            vec![LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -2,
                hi_extent: 4,
            }]
        );
    }

    #[test]
    fn fold_ptr_add_checked_shifts_second_window_by_first_delta() {
        // First op moves r13 by +2 within window [0, 3]; second op (measured
        // against new r13) has window [-1, 2] which is [1, 4] measured against
        // initial r13. Combined: delta = 2, lo = min(0, -1+2) = 0 (actually
        // min(0, 2 + (-1)) = min(0, 1) = 0), hi = max(3, 2 + 2) = 4.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 2,
                lo_extent: 0,
                hi_extent: 3,
            },
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -1,
                hi_extent: 2,
            },
        ]);
        assert_eq!(
            out,
            vec![LirInst::PtrAddChecked {
                delta: 2,
                lo_extent: 0,
                hi_extent: 4,
            }]
        );
    }

    #[test]
    fn fold_ptr_add_checked_drops_if_combined_is_noop() {
        // Two degenerate ops (window {0}, delta 0) collapse to nothing.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 0,
            },
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 0,
            },
        ]);
        assert!(out.is_empty());
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

    #[test]
    fn fold_cell_set_zero_then_cell_set_at_one_zero_becomes_zero_run() {
        let out = run(vec![
            LirInst::CellSet(0),
            LirInst::CellSetAt { off: 1, val: 0 },
        ]);
        assert_eq!(out, vec![LirInst::ZeroRun { start: 0, count: 2 }]);
    }

    #[test]
    fn fold_cell_set_at_minus_one_then_cell_set_zero_becomes_zero_run() {
        let out = run(vec![
            LirInst::CellSetAt { off: -1, val: 0 },
            LirInst::CellSet(0),
        ]);
        assert_eq!(
            out,
            vec![LirInst::ZeroRun {
                start: -1,
                count: 2
            }]
        );
    }

    #[test]
    fn fold_adjacent_cell_set_at_zeros_becomes_zero_run() {
        let out = run(vec![
            LirInst::CellSetAt { off: 5, val: 0 },
            LirInst::CellSetAt { off: 6, val: 0 },
        ]);
        assert_eq!(out, vec![LirInst::ZeroRun { start: 5, count: 2 }]);
    }

    #[test]
    fn fold_extends_existing_zero_run_forward() {
        let out = run(vec![
            LirInst::CellSet(0),
            LirInst::CellSetAt { off: 1, val: 0 },
            LirInst::CellSetAt { off: 2, val: 0 },
            LirInst::CellSetAt { off: 3, val: 0 },
        ]);
        assert_eq!(out, vec![LirInst::ZeroRun { start: 0, count: 4 }]);
    }

    #[test]
    fn fold_extends_existing_zero_run_from_negative_into_origin() {
        let out = run(vec![
            LirInst::CellSetAt { off: -2, val: 0 },
            LirInst::CellSetAt { off: -1, val: 0 },
            LirInst::CellSet(0),
        ]);
        assert_eq!(
            out,
            vec![LirInst::ZeroRun {
                start: -2,
                count: 3
            }]
        );
    }

    #[test]
    fn fold_zero_writes_with_gap_stay_separate() {
        // Offsets 0 and 2 are not contiguous → no ZeroRun.
        let out = run(vec![
            LirInst::CellSet(0),
            LirInst::CellSetAt { off: 2, val: 0 },
        ]);
        assert_eq!(
            out,
            vec![LirInst::CellSet(0), LirInst::CellSetAt { off: 2, val: 0 },]
        );
    }

    #[test]
    fn fold_non_zero_set_does_not_join_zero_run() {
        // A non-zero Set must not merge into a ZeroRun — different byte value.
        let out = run(vec![
            LirInst::CellSet(0),
            LirInst::CellSetAt { off: 1, val: 0 },
            LirInst::CellSetAt { off: 2, val: 7 },
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::ZeroRun { start: 0, count: 2 },
                LirInst::CellSetAt { off: 2, val: 7 },
            ]
        );
    }

    #[test]
    fn fold_does_not_create_zero_run_across_barrier() {
        let out = run(vec![
            LirInst::CellSet(0),
            LirInst::PutByte,
            LirInst::CellSetAt { off: 1, val: 0 },
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::CellSet(0),
                LirInst::PutByte,
                LirInst::CellSetAt { off: 1, val: 0 },
            ]
        );
    }
}
