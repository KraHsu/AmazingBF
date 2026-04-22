//! Promote `Scan(dir)` to `ScanWithHint { dir, hint_bytes }` when a preceding
//! `PtrAddChecked` has already verified that `hint_bytes` cells in `dir` from
//! the current `r13` are mapped inside the tape.
//!
//! The pass walks LIR with the same `verified_window` state machine the
//! backend uses (see `crate::backend::codegen`), so the two views of "what's
//! already been proven mapped" stay consistent. The window lives between a
//! `PtrAddChecked` and the next barrier; `PtrAdd` inside the window shifts
//! the origin, `CellAdd*` / `CellSet*` are transparent, everything else
//! clears it.
//!
//! For `Scan(+1)` the hint is the current window's `hi_extent` (clamped at
//! 0); for `Scan(-1)` the hint is `-lo_extent` (clamped at 0). A hint of 0
//! means no speedup is available and the `Scan` is left as-is so codegen
//! stays on the slow path rather than set up an unused limit register.

use crate::ir::lir::{LirInst, LirProgram};

/// Walk `program` and lift `Scan` to `ScanWithHint` wherever the preceding
/// `PtrAddChecked` window has already covered the first-iteration target.
pub(crate) fn promote_scan_hints(program: LirProgram) -> LirProgram {
    let mut out = Vec::with_capacity(program.insts.len());
    let mut window: Option<(isize, isize)> = None;

    for inst in program.insts {
        match &inst {
            LirInst::Scan(dir) => {
                let step = *dir;
                let hint_cells = match window {
                    Some((_, hi)) if step == 1 && hi > 0 => hi,
                    Some((lo, _)) if step == -1 && lo < 0 => -lo,
                    _ => 0,
                };
                if hint_cells > 0 {
                    let hint_bytes = u32::try_from(hint_cells).unwrap_or(u32::MAX);
                    out.push(LirInst::ScanWithHint {
                        dir: step,
                        hint_bytes,
                    });
                } else {
                    out.push(inst);
                }
                window = None;
            }
            LirInst::PtrAddChecked {
                delta,
                lo_extent,
                hi_extent,
            } => {
                let (nlo, nhi) = match window {
                    Some((wlo, whi)) => (wlo.min(*lo_extent), whi.max(*hi_extent)),
                    None => (*lo_extent, *hi_extent),
                };
                window = Some((nlo - delta, nhi - delta));
                out.push(inst);
            }
            LirInst::PtrAdd(n) => {
                let n = *n;
                if let Some((wlo, whi)) = window
                    && wlo <= n
                    && n <= whi
                {
                    window = Some((wlo - n, whi - n));
                } else {
                    window = None;
                }
                out.push(inst);
            }
            // Cell writes never touch r13, so the verified window survives.
            LirInst::CellAdd(_)
            | LirInst::CellSet(_)
            | LirInst::CellAddAt { .. }
            | LirInst::CellSetAt { .. }
            | LirInst::ZeroRun { .. } => {
                out.push(inst);
            }
            // Everything else (labels, jumps, I/O, LinearMul, existing
            // ScanWithHint, etc.) is a barrier.
            _ => {
                window = None;
                out.push(inst);
            }
        }
    }

    LirProgram { insts: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lir::{LabelId, LirInst};

    fn run(insts: Vec<LirInst>) -> Vec<LirInst> {
        promote_scan_hints(LirProgram { insts }).insts
    }

    #[test]
    fn scan_without_preceding_checked_stays_bare() {
        let out = run(vec![LirInst::Scan(1)]);
        assert_eq!(out, vec![LirInst::Scan(1)]);
    }

    #[test]
    fn right_scan_after_high_side_check_gets_hint() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 5,
            },
            LirInst::Scan(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: 0,
                    hi_extent: 5,
                },
                LirInst::ScanWithHint {
                    dir: 1,
                    hint_bytes: 5,
                },
            ]
        );
    }

    #[test]
    fn left_scan_after_low_side_check_gets_hint() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -4,
                hi_extent: 0,
            },
            LirInst::Scan(-1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -4,
                    hi_extent: 0,
                },
                LirInst::ScanWithHint {
                    dir: -1,
                    hint_bytes: 4,
                },
            ]
        );
    }

    #[test]
    fn scan_opposite_direction_gets_no_hint() {
        // Window covers only the left side, so a right-going Scan can't use it.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -4,
                hi_extent: 0,
            },
            LirInst::Scan(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAddChecked {
                    delta: 0,
                    lo_extent: -4,
                    hi_extent: 0,
                },
                LirInst::Scan(1),
            ]
        );
    }

    #[test]
    fn ptr_add_inside_window_shifts_origin_for_hint() {
        // Window [-2, 5]; PtrAdd(3) shifts to [-5, 2]; right-scan hint becomes 2.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -2,
                hi_extent: 5,
            },
            LirInst::PtrAdd(3),
            LirInst::Scan(1),
        ]);
        assert_eq!(out.len(), 3);
        assert!(matches!(
            out[2],
            LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 2,
            }
        ));
    }

    #[test]
    fn ptr_add_escaping_window_clears_hint() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 5,
            },
            LirInst::PtrAdd(10),
            LirInst::Scan(1),
        ]);
        assert_eq!(
            out.last(),
            Some(&LirInst::Scan(1)),
            "Scan must stay bare after PtrAdd escapes the verified window"
        );
    }

    #[test]
    fn cell_writes_are_transparent_to_hint_tracking() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 3,
            },
            LirInst::CellAdd(1),
            LirInst::CellSet(0),
            LirInst::CellAddAt { off: 1, delta: 1 },
            LirInst::CellSetAt { off: 2, val: 5 },
            LirInst::Scan(1),
        ]);
        assert!(matches!(
            out.last(),
            Some(&LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 3,
            })
        ));
    }

    #[test]
    fn zero_run_is_transparent_to_hint_tracking() {
        // A ZeroRun writes to several cells but does not touch r13, so the
        // verified window must survive across it.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 5,
            },
            LirInst::ZeroRun { start: 0, count: 3 },
            LirInst::Scan(1),
        ]);
        assert!(matches!(
            out.last(),
            Some(&LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 5,
            })
        ));
    }

    #[test]
    fn label_barrier_clears_window() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 5,
            },
            LirInst::Label(LabelId(0)),
            LirInst::Scan(1),
        ]);
        assert_eq!(out.last(), Some(&LirInst::Scan(1)));
    }

    #[test]
    fn putbyte_barrier_clears_window() {
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 5,
            },
            LirInst::PutByte,
            LirInst::Scan(1),
        ]);
        assert_eq!(out.last(), Some(&LirInst::Scan(1)));
    }

    #[test]
    fn multiple_checked_ops_union_windows_for_hint() {
        // Window [0,3] then extended by [2,5] → [0,5]; right-scan gets 5.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 0,
                hi_extent: 3,
            },
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: 2,
                hi_extent: 5,
            },
            LirInst::Scan(1),
        ]);
        assert!(matches!(
            out.last(),
            Some(&LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 5,
            })
        ));
    }

    #[test]
    fn scan_itself_is_a_barrier_for_following_scan() {
        // First Scan gets a hint, but clears the window; the second Scan has
        // nothing to consume and stays bare.
        let out = run(vec![
            LirInst::PtrAddChecked {
                delta: 0,
                lo_extent: -3,
                hi_extent: 3,
            },
            LirInst::Scan(1),
            LirInst::Scan(-1),
        ]);
        assert_eq!(out[2], LirInst::Scan(-1));
    }
}
