//! Abstract interpretation of HIR fragments: pointer delta range, touched
//! cells, read/write/IO summary.
//!
//! [`LoopEffect::analyze`] walks any straight-line or nested HIR fragment
//! and reports a conservative summary suitable for loop specialisation
//! (B3 generalised `LinearMul`), bounds-check batching (C2), and scheduling
//! decisions in later passes.
//!
//! `touched` is a half-open range relative to the fragment's entry pointer.
//! The empty range `0..0` means "nothing touched"; the sentinel
//! `isize::MIN..isize::MAX` means "touched set is not statically bounded".
//!
//! `net_ptr_delta == None` means the fragment's net pointer offset is not
//! statically determinable (e.g. a `Scan`, or a `Loop` whose body has a
//! non-zero per-iteration delta — the iteration count is unknown).
//!
//! `reads_cell` / `writes_cell` / `has_io` are set whenever any inner
//! instruction reads the current cell, writes any cell, or performs I/O.
//! For `Loop`, the while-non-zero check itself counts as a read of the
//! entry cell.
//!
//! Consumed by `TapeState::apply` (B5 selective clobber) and future passes.

use std::ops::Range;

use crate::ir::hir::HirInst;

/// Conservative summary of a HIR fragment's side effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoopEffect {
    /// Net pointer offset after executing the fragment once, relative to
    /// entry. `None` means the fragment contains control flow whose
    /// iteration count cannot be determined statically.
    pub(crate) net_ptr_delta: Option<isize>,
    /// Half-open range of cell offsets the fragment reads or writes,
    /// relative to the entry pointer. `0..0` ⇒ nothing touched;
    /// `isize::MIN..isize::MAX` ⇒ unbounded.
    pub(crate) touched: Range<isize>,
    /// Any inner instruction reads a cell (including the implicit
    /// loop-condition read of a `Loop`).
    pub(crate) reads_cell: bool,
    /// Any inner instruction writes a cell.
    pub(crate) writes_cell: bool,
    /// Any inner instruction performs I/O (`PutByte` / `GetByte`).
    pub(crate) has_io: bool,
}

fn empty_range() -> Range<isize> {
    0..0
}

fn unbounded_range() -> Range<isize> {
    isize::MIN..isize::MAX
}

fn is_unbounded(r: &Range<isize>) -> bool {
    r.start == isize::MIN && r.end == isize::MAX
}

fn union(a: Range<isize>, b: Range<isize>) -> Range<isize> {
    if is_unbounded(&a) || is_unbounded(&b) {
        return unbounded_range();
    }
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    a.start.min(b.start)..a.end.max(b.end)
}

fn shift(r: Range<isize>, by: isize) -> Range<isize> {
    if is_unbounded(&r) || r.is_empty() {
        return r;
    }
    let start = r.start.saturating_add(by);
    let end = r.end.saturating_add(by);
    start..end
}

fn single_cell(off: isize) -> Range<isize> {
    off..off.saturating_add(1)
}

impl LoopEffect {
    /// Walk `insts` once and return a conservative [`LoopEffect`].
    ///
    /// `Loop` bodies are analysed recursively; an outer `Loop` only retains
    /// a bounded touched range when its body has `net_ptr_delta == Some(0)`
    /// (the loop revisits the same window each iteration).
    pub(crate) fn analyze(insts: &[HirInst]) -> Self {
        let mut cur_ptr: isize = 0;
        let mut ptr_known = true;
        let mut touched = empty_range();
        let mut reads = false;
        let mut writes = false;
        let mut io = false;

        let touch_current = |touched: &mut Range<isize>, ptr_known: bool, cur_ptr: isize| {
            if ptr_known {
                *touched = union(touched.clone(), single_cell(cur_ptr));
            } else {
                *touched = unbounded_range();
            }
        };

        for inst in insts {
            match inst {
                HirInst::Move(d) => {
                    if ptr_known {
                        cur_ptr = cur_ptr.saturating_add(*d);
                    }
                }
                HirInst::Add(_) => {
                    reads = true;
                    writes = true;
                    touch_current(&mut touched, ptr_known, cur_ptr);
                }
                HirInst::Zero => {
                    writes = true;
                    touch_current(&mut touched, ptr_known, cur_ptr);
                }
                HirInst::PutByte => {
                    reads = true;
                    io = true;
                    touch_current(&mut touched, ptr_known, cur_ptr);
                }
                HirInst::GetByte => {
                    writes = true;
                    io = true;
                    touch_current(&mut touched, ptr_known, cur_ptr);
                }
                HirInst::LinearMul(factors) => {
                    reads = true;
                    writes = true;
                    if ptr_known {
                        touched = union(touched, single_cell(cur_ptr));
                        for (off, _factor) in factors {
                            touched = union(touched, single_cell(cur_ptr.saturating_add(*off)));
                        }
                    } else {
                        touched = unbounded_range();
                    }
                    // Per HIR semantics the pointer is unchanged.
                }
                HirInst::LinearMulWithSets { factors, sets, .. } => {
                    reads = true;
                    writes = true;
                    if ptr_known {
                        touched = union(touched, single_cell(cur_ptr));
                        for (off, _) in factors {
                            touched = union(touched, single_cell(cur_ptr.saturating_add(*off)));
                        }
                        for off in sets {
                            touched = union(touched, single_cell(cur_ptr.saturating_add(*off)));
                        }
                    } else {
                        touched = unbounded_range();
                    }
                }
                HirInst::Scan(_dir) => {
                    reads = true;
                    touched = unbounded_range();
                    ptr_known = false;
                }
                HirInst::Loop(body) => {
                    let body_eff = LoopEffect::analyze(body);
                    // Loop condition reads the entry cell of the body.
                    reads = true;
                    reads |= body_eff.reads_cell;
                    writes |= body_eff.writes_cell;
                    io |= body_eff.has_io;
                    touch_current(&mut touched, ptr_known, cur_ptr);

                    if !ptr_known || is_unbounded(&body_eff.touched) {
                        touched = unbounded_range();
                        ptr_known = false;
                    } else {
                        match body_eff.net_ptr_delta {
                            Some(0) => {
                                touched = union(touched, shift(body_eff.touched, cur_ptr));
                                // Pointer sticks at cur_ptr across iterations.
                            }
                            _ => {
                                // Non-zero or unknown per-iteration delta ⇒ pointer
                                // wanders across an indeterminate number of iterations.
                                touched = unbounded_range();
                                ptr_known = false;
                            }
                        }
                    }
                }
            }
        }

        LoopEffect {
            net_ptr_delta: if ptr_known { Some(cur_ptr) } else { None },
            touched,
            reads_cell: reads,
            writes_cell: writes,
            has_io: io,
        }
    }
}

/// Pointer trace of a straight-line fragment: `(min_off, max_off, net_delta)`
/// relative to the entry, where `net_delta == None` iff the fragment contains
/// a `Scan` or a `Loop` whose body does not have `net_ptr_delta == Some(0)`.
/// `min_off` / `max_off` cover every position the pointer holds *between*
/// instructions, including the entry position `0`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn pointer_delta_range(insts: &[HirInst]) -> (isize, isize, Option<isize>) {
    let mut cur: isize = 0;
    let mut ptr_known = true;
    let mut lo = 0isize;
    let mut hi = 0isize;

    for inst in insts {
        match inst {
            HirInst::Move(d) => {
                if ptr_known {
                    cur = cur.saturating_add(*d);
                    if cur < lo {
                        lo = cur;
                    }
                    if cur > hi {
                        hi = cur;
                    }
                }
            }
            HirInst::Scan(_) => {
                ptr_known = false;
            }
            HirInst::Loop(body) => {
                let body_eff = LoopEffect::analyze(body);
                if !matches!(body_eff.net_ptr_delta, Some(0)) {
                    ptr_known = false;
                }
            }
            _ => { /* pointer unchanged */ }
        }
    }

    (lo, hi, if ptr_known { Some(cur) } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_touched() -> Range<isize> {
        unbounded_range()
    }

    #[test]
    fn balanced_copy_loop_body() {
        // Body of `[->+<]` after HIR lowering: `Add(-1), Move(1), Add(1), Move(-1)`.
        let body = vec![
            HirInst::Add(-1),
            HirInst::Move(1),
            HirInst::Add(1),
            HirInst::Move(-1),
        ];
        let eff = LoopEffect::analyze(&body);
        assert_eq!(eff.net_ptr_delta, Some(0));
        assert_eq!(eff.touched, 0..2);
        assert!(eff.reads_cell);
        assert!(eff.writes_cell);
        assert!(!eff.has_io);
    }

    #[test]
    fn scan_body_has_pointer_delta() {
        // Body of `[>]` is a single `Move(1)` — it neither reads nor writes.
        let body = vec![HirInst::Move(1)];
        let eff = LoopEffect::analyze(&body);
        assert_eq!(eff.net_ptr_delta, Some(1));
        assert!(eff.touched.is_empty());
        assert!(!eff.reads_cell);
        assert!(!eff.writes_cell);
    }

    #[test]
    fn outer_loop_of_scan_is_unbounded() {
        // `[>]` as a whole: outer Loop body = `[Move(1)]`. Per-iteration delta is +1,
        // so iteration count is unknown and pointer wanders.
        let frag = vec![HirInst::Loop(vec![HirInst::Move(1)])];
        let eff = LoopEffect::analyze(&frag);
        assert_eq!(eff.net_ptr_delta, None);
        assert_eq!(eff.touched, unknown_touched());
        assert!(eff.reads_cell); // loop condition reads entry cell
        assert!(!eff.writes_cell);
        assert!(!eff.has_io);
    }

    #[test]
    fn nested_unbalanced_loop_poisons_outer_ptr() {
        // `[[>]+]` → outer body = `[Loop([Move(1)]), Add(1)]`.
        let frag = vec![HirInst::Loop(vec![HirInst::Move(1)]), HirInst::Add(1)];
        let eff = LoopEffect::analyze(&frag);
        assert_eq!(eff.net_ptr_delta, None);
        assert_eq!(eff.touched, unknown_touched());
        assert!(eff.reads_cell);
        assert!(eff.writes_cell);
        assert!(!eff.has_io);
    }

    #[test]
    fn put_byte_propagates_io_flag() {
        let frag = vec![HirInst::Loop(vec![HirInst::PutByte])];
        let eff = LoopEffect::analyze(&frag);
        assert!(eff.has_io);
        assert!(eff.reads_cell);
    }

    #[test]
    fn get_byte_sets_writes_and_io() {
        let frag = vec![HirInst::GetByte];
        let eff = LoopEffect::analyze(&frag);
        assert!(eff.has_io);
        assert!(eff.writes_cell);
        assert_eq!(eff.net_ptr_delta, Some(0));
        assert_eq!(eff.touched, 0..1);
    }

    #[test]
    fn linear_mul_covers_factor_offsets() {
        // `LinearMul([(1, 1), (3, 2)])` writes cells 0/1/3 (0 is the head that gets zeroed).
        let frag = vec![HirInst::LinearMul(vec![(1, 1), (3, 2)])];
        let eff = LoopEffect::analyze(&frag);
        assert_eq!(eff.net_ptr_delta, Some(0));
        assert_eq!(eff.touched, 0..4);
        assert!(eff.reads_cell);
        assert!(eff.writes_cell);
    }

    #[test]
    fn pointer_delta_range_tracks_min_max_and_net() {
        let frag = vec![HirInst::Move(3), HirInst::Move(-1), HirInst::Move(2)];
        assert_eq!(pointer_delta_range(&frag), (0, 4, Some(4)));
    }

    #[test]
    fn pointer_delta_range_spans_negative_offsets() {
        let frag = vec![HirInst::Move(-2), HirInst::Move(5)];
        assert_eq!(pointer_delta_range(&frag), (-2, 3, Some(3)));
    }

    #[test]
    fn pointer_delta_range_poisons_on_scan() {
        let frag = vec![HirInst::Move(1), HirInst::Scan(1)];
        let (lo, hi, net) = pointer_delta_range(&frag);
        assert_eq!((lo, hi), (0, 1));
        assert_eq!(net, None);
    }

    #[test]
    fn pointer_delta_range_poisons_on_unbalanced_loop() {
        let frag = vec![HirInst::Loop(vec![HirInst::Move(1)])];
        let (_, _, net) = pointer_delta_range(&frag);
        assert_eq!(net, None);
    }

    #[test]
    fn pointer_delta_range_survives_balanced_loop() {
        // Balanced loop: body net delta is 0, so pointer stays at 0 afterwards.
        let frag = vec![HirInst::Loop(vec![
            HirInst::Add(-1),
            HirInst::Move(1),
            HirInst::Add(1),
            HirInst::Move(-1),
        ])];
        assert_eq!(pointer_delta_range(&frag), (0, 0, Some(0)));
    }

    #[test]
    fn empty_fragment_has_zero_effect() {
        let eff = LoopEffect::analyze(&[]);
        assert_eq!(eff.net_ptr_delta, Some(0));
        assert!(eff.touched.is_empty());
        assert!(!eff.reads_cell);
        assert!(!eff.writes_cell);
        assert!(!eff.has_io);
    }
}
