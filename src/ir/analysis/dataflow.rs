//! Forward dataflow framework for HIR fragments.
//!
//! HIR is a tree, not a basic-block CFG — `run_forward` walks the tree in
//! program order, invoking [`Transfer::transfer_inst`] for scalar opcodes
//! and [`Transfer::transfer_loop`] for `HirInst::Loop`. Implementers
//! customise the transfer for their fact domain; the default
//! `transfer_loop` runs a bounded fixed-point iteration, meeting the
//! loop-entry fact with the body-exit fact until either they stabilise or
//! `MAX_LOOP_ITERS` is exhausted (in which case the fact retreats to
//! [`Fact::bottom`], preserving soundness).
//!
//! The bound is independent of the outer `optimize_o2` fixed-point cap —
//! it governs per-loop fact convergence, not whole-program IR rewriting.
//! For short lattices (`CellLattice`: height 4) convergence typically
//! lands in 3–5 iterations.
//!
//! Until a consumer pass lands this module has no non-test users, so the
//! module-level `allow(dead_code)` keeps the build clean. Remove it when
//! `optimize.rs` (or a later HIR pass) references `run_forward`.

#![cfg_attr(not(test), allow(dead_code))]

use crate::ir::analysis::tape_state::TapeState;
use crate::ir::hir::HirInst;

/// Per-loop fixed-point iteration budget. Upper bound only — every lattice
/// used with this framework must have finite height, so real convergence is
/// always well below this cap.
const MAX_LOOP_ITERS: usize = 64;

/// A join-semilattice fact with a designated bottom element.
pub(crate) trait Fact: Clone + PartialEq {
    /// The bottom element (least informative; for optional facts, `None`
    /// conventionally represents "unreachable").
    fn bottom() -> Self;
    /// In-place lattice meet with `other`. After this call `self` must be
    /// ≥ both the prior `self` and `other`.
    fn meet_with(&mut self, other: &Self);
}

/// Transfer function for a forward analysis.
pub(crate) trait Transfer<F: Fact> {
    /// Apply one non-Loop HIR instruction.
    fn transfer_inst(&self, fact: &mut F, inst: &HirInst);

    /// Summarise the effect of a `Loop` body on `fact_in`. Default: iterate
    /// `meet(entry, body_end)` to a fixed point, bounded by
    /// `MAX_LOOP_ITERS`; retreat to [`Fact::bottom`] if the bound is hit.
    fn transfer_loop(&self, fact_in: &F, body: &[HirInst]) -> F {
        let mut entry = fact_in.clone();
        for _ in 0..MAX_LOOP_ITERS {
            let mut body_end = entry.clone();
            for inst in body {
                match inst {
                    HirInst::Loop(nested) => {
                        body_end = self.transfer_loop(&body_end, nested);
                    }
                    other => self.transfer_inst(&mut body_end, other),
                }
            }
            let mut new_entry = entry.clone();
            new_entry.meet_with(&body_end);
            if new_entry == entry {
                return new_entry;
            }
            entry = new_entry;
        }
        F::bottom()
    }
}

/// Walk `insts` in order, threading `entry` through the transfer function.
pub(crate) fn run_forward<F: Fact, T: Transfer<F>>(insts: &[HirInst], entry: F, xfer: &T) -> F {
    let mut fact = entry;
    for inst in insts {
        match inst {
            HirInst::Loop(body) => {
                fact = xfer.transfer_loop(&fact, body);
            }
            other => xfer.transfer_inst(&mut fact, other),
        }
    }
    fact
}

impl Fact for Option<TapeState> {
    fn bottom() -> Self {
        None
    }
    fn meet_with(&mut self, other: &Self) {
        match (self.as_mut(), other.as_ref()) {
            (_, None) => {}
            (None, Some(o)) => {
                *self = Some(o.clone());
            }
            (Some(a), Some(b)) => a.merge_in_place(b),
        }
    }
}

/// `Fact` impl for raw [`TapeState`]. Used by consumer passes that operate
/// on always-reachable HIR fragments (e.g. `optimize_o1`) and therefore do
/// not need the `Option` wrapper's unreachable-vs-unknown distinction.
/// `bottom()` is the maximally-weak "we know nothing" state — a fresh
/// block entry with no cell facts.
impl Fact for TapeState {
    fn bottom() -> Self {
        TapeState::new_block()
    }
    fn meet_with(&mut self, other: &Self) {
        self.merge_in_place(other);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::analysis::lattice::CellLattice;

    /// Minimal const-propagation transfer: delegates to `TapeState::apply`.
    /// A `None` fact (unreachable) stays `None`.
    struct ConstPropXfer;

    impl Transfer<Option<TapeState>> for ConstPropXfer {
        fn transfer_inst(&self, fact: &mut Option<TapeState>, inst: &HirInst) {
            if let Some(st) = fact.as_mut() {
                st.apply(inst);
            }
        }
    }

    #[test]
    fn run_forward_propagates_sequential_facts() {
        let frag = vec![
            HirInst::Zero,
            HirInst::Add(3),
            HirInst::Move(1),
            HirInst::Add(2),
        ];
        let entry: Option<TapeState> = Some(TapeState::new_program());
        let out = run_forward(&frag, entry, &ConstPropXfer);
        let st = out.expect("reachable");
        assert_eq!(st.lattice_at(0), CellLattice::Const(3));
        // cell 1 was unknown before Add(2); literal-equivalence transfer
        // leaves Top as Top.
        assert_eq!(st.lattice_at(1), CellLattice::Top);
    }

    #[test]
    fn run_forward_loop_with_nonzero_body_delta_becomes_pessimistic() {
        // Body = [Add(1), Move(1)] — net_ptr_delta != 0, so entry/body-end
        // ptrs diverge and the meet collapses to pessimistic.
        let frag = vec![HirInst::Loop(vec![HirInst::Add(1), HirInst::Move(1)])];
        let entry: Option<TapeState> = Some(TapeState::new_program());
        let out = run_forward(&frag, entry, &ConstPropXfer);
        let st = out.expect("reachable");
        assert!(st.is_pessimistic());
        assert_eq!(st.lattice_at(0), CellLattice::Top);
        assert_eq!(st.lattice_at(5), CellLattice::Top);
    }

    #[test]
    fn run_forward_loop_with_zero_body_delta_preserves_entry_fact() {
        // Body = [Zero] — idempotent; entry and body-end agree on ptr and
        // on cell[0]=Zero, so the fixed point is the entry fact itself.
        let frag = vec![HirInst::Loop(vec![HirInst::Zero])];
        let entry: Option<TapeState> = Some(TapeState::new_program());
        let out = run_forward(&frag, entry, &ConstPropXfer);
        let st = out.expect("reachable");
        assert!(!st.is_pessimistic());
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
    }

    #[test]
    fn run_forward_deeply_nested_loops_converge() {
        // Build 10 levels of `Loop([...])`, innermost body empty. Each
        // transfer_loop invocation on an empty body converges in one
        // iteration (body_end == entry).
        let mut inst: HirInst = HirInst::Loop(vec![]);
        for _ in 0..10 {
            inst = HirInst::Loop(vec![inst]);
        }
        let frag = vec![inst];
        let entry: Option<TapeState> = Some(TapeState::new_program());
        let out = run_forward(&frag, entry, &ConstPropXfer);
        // All the loops have net-zero body delta and no writes, so the
        // initial fact survives.
        let st = out.expect("reachable");
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
    }

    #[test]
    fn run_forward_unreachable_entry_stays_unreachable() {
        let frag = vec![HirInst::Zero, HirInst::Add(1)];
        let out: Option<TapeState> = run_forward(&frag, None, &ConstPropXfer);
        assert!(out.is_none());
    }

    #[test]
    fn fact_meet_with_none_is_identity() {
        let mut a: Option<TapeState> = Some(TapeState::new_program());
        a.meet_with(&None);
        assert!(a.is_some());
        let none_b = a.clone();
        let mut c: Option<TapeState> = None;
        c.meet_with(&none_b);
        assert!(c.is_some());
    }
}
