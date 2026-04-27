//! Per-block symbolic tape state.
//!
//! Tracks the data pointer offset (relative to the block entry) and a sparse
//! map of cell facts expressed in the [`CellLattice`] four-point domain.
//! `apply` transfers a single [`HirInst`] through the lattice, delegating
//! the `Add(k)` case to [`CellLattice::add_wrapping`] so the four lattice
//! points (`Top` / `NonZero` / `Zero` / `Const(_)`) propagate consistently.
//!
//! In a straight-line fragment seeded from `new_program()` or `new_block()`
//! the only lattice points reachable are `Top`, `Zero`, and `Const(_)`, so
//! the behaviour is byte-for-byte identical to the older `ConstEnv`.
//! `NonZero` only materialises via [`merge_in_place`] at control-flow joins,
//! which is where the richer transfer actually pays off.
//!
//! Absence of a key in [`TapeState::cells`] represents `Top` (unknown).

use std::collections::BTreeMap;

use crate::ir::analysis::lattice::CellLattice;
use crate::ir::analysis::loop_effect::LoopEffect;
use crate::ir::hir::HirInst;

/// Symbolic tape state for a straight-line HIR fragment.
///
/// `pessimistic == true` encodes "we know nothing": any query returns `Top`
/// and [`apply`] is an inert no-op. The flag is only set by [`merge_in_place`]
/// when two facts disagree on the pointer offset (which destroys the frame
/// of reference for per-offset cell facts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TapeState {
    /// Data pointer offset relative to the block entry. Not meaningful when
    /// `pessimistic`.
    ptr: isize,
    /// Sparse map of offset → lattice fact. Missing key ⇒ `Top`.
    cells: BTreeMap<isize, CellLattice>,
    /// `true` iff a prior merge dropped the state to "no trustworthy facts".
    pessimistic: bool,
}

impl TapeState {
    /// Entry state for a nested block: pointer at 0, no known cells.
    pub(crate) fn new_block() -> Self {
        Self {
            ptr: 0,
            cells: BTreeMap::new(),
            pessimistic: false,
        }
    }

    /// Entry state for the whole program: tape is zero, so `cell[0] = Zero`.
    pub(crate) fn new_program() -> Self {
        let mut cells = BTreeMap::new();
        cells.insert(0, CellLattice::Zero);
        Self {
            ptr: 0,
            cells,
            pessimistic: false,
        }
    }

    /// Known byte value at the current pointer, or `None` if unknown / non-zero.
    pub(crate) fn value_at_ptr(&self) -> Option<u8> {
        self.value_at(self.ptr)
    }

    /// Known byte value at an absolute offset, or `None` if unknown / non-zero.
    pub(crate) fn value_at(&self, off: isize) -> Option<u8> {
        self.lattice_at(off).known_u8()
    }

    /// Lattice fact at an absolute offset; missing keys read as `Top`.
    /// A pessimistic state returns `Top` everywhere.
    ///
    /// Public query for future A4a consumers; `#[allow(dead_code)]` on the
    /// non-test build until a `Fact`/`Transfer` pass references it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn lattice_at(&self, off: isize) -> CellLattice {
        if self.pessimistic {
            return CellLattice::Top;
        }
        self.cells.get(&off).copied().unwrap_or(CellLattice::Top)
    }

    /// `true` iff this state carries no trustworthy per-cell facts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_pessimistic(&self) -> bool {
        self.pessimistic
    }

    /// Merge `other` into `self` via lattice meet. Produces the weakest fact
    /// that conservatively covers both inputs. Ptr-mismatch collapses to a
    /// pessimistic state because per-offset facts only make sense relative
    /// to a shared pointer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn merge_in_place(&mut self, other: &Self) {
        if self.pessimistic {
            return;
        }
        if other.pessimistic || self.ptr != other.ptr {
            self.pessimistic = true;
            self.cells.clear();
            return;
        }
        self.cells.retain(|off, fact| {
            let merged = fact.meet(other.lattice_at(*off));
            *fact = merged;
            !matches!(merged, CellLattice::Top)
        });
    }

    /// `true` iff the current cell is provably zero.
    ///
    /// Public query for future A4a consumers; `#[allow(dead_code)]` on the
    /// non-test build until a consumer references it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_known_zero_at_ptr(&self) -> bool {
        self.lattice_at(self.ptr).is_zero()
    }

    /// Drop every known cell fact but keep `ptr`. Used when control flow or
    /// unanalysed control structures (`Loop` / `Scan`) render prior cell
    /// facts unreliable. `LinearMul` / `LinearMulWithSets` have precise
    /// transfer functions and do not use this path.
    pub(crate) fn clobber_all(&mut self) {
        self.cells.clear();
    }

    /// Transfer a single HIR instruction. No-op on pessimistic states.
    pub(crate) fn apply(&mut self, inst: &HirInst) {
        if self.pessimistic {
            return;
        }
        match inst {
            HirInst::Move(d) => self.ptr += *d,
            HirInst::Add(k) => {
                let current = self.lattice_at(self.ptr);
                let next = current.add_wrapping(*k);
                if matches!(next, CellLattice::Top) {
                    self.cells.remove(&self.ptr);
                } else {
                    self.cells.insert(self.ptr, next);
                }
            }
            HirInst::Zero => {
                self.cells.insert(self.ptr, CellLattice::Zero);
            }
            HirInst::GetByte => {
                self.cells.remove(&self.ptr);
            }
            HirInst::PutByte => { /* pure side effect on stdout */ }
            HirInst::LinearMul(factors) => {
                let head_val = self.lattice_at(self.ptr);
                self.cells.insert(self.ptr, CellLattice::Zero);
                for &(off, f) in factors {
                    let abs_off = self.ptr + off;
                    match head_val.known_u8() {
                        Some(v) => {
                            let delta = (v as i32).wrapping_mul(f);
                            let cur = self.lattice_at(abs_off);
                            let next = cur.add_wrapping(delta);
                            if matches!(next, CellLattice::Top) {
                                self.cells.remove(&abs_off);
                            } else {
                                self.cells.insert(abs_off, next);
                            }
                        }
                        None => {
                            self.cells.remove(&abs_off);
                        }
                    }
                }
            }
            HirInst::LinearMulWithSets { factors, sets } => {
                let head_lat = self.lattice_at(self.ptr);
                if head_lat.is_zero() {
                    // v == 0 → entire instruction is a no-op.
                } else {
                    self.cells.insert(self.ptr, CellLattice::Zero);
                    let head_known = head_lat.known_u8();
                    for &(off, f) in factors {
                        let abs_off = self.ptr + off;
                        match head_known {
                            Some(v) => {
                                let delta = (v as i32).wrapping_mul(f);
                                let cur = self.lattice_at(abs_off);
                                let next = cur.add_wrapping(delta);
                                if matches!(next, CellLattice::Top) {
                                    self.cells.remove(&abs_off);
                                } else {
                                    self.cells.insert(abs_off, next);
                                }
                            }
                            None => {
                                self.cells.remove(&abs_off);
                            }
                        }
                    }
                    for &off in sets {
                        let abs_off = self.ptr + off;
                        if head_lat.is_nonzero() {
                            self.cells.insert(abs_off, CellLattice::Zero);
                        } else {
                            // head is Top: sets may or may not fire
                            self.cells.remove(&abs_off);
                        }
                    }
                }
            }
            HirInst::Scan(_) => {
                self.clobber_all();
            }
            HirInst::Loop(body) => {
                let eff = LoopEffect::analyze(body);
                if eff.touched.start == isize::MIN && eff.touched.end == isize::MAX {
                    self.clobber_all();
                } else if matches!(eff.net_ptr_delta, Some(0)) {
                    self.cells.retain(|off, _| {
                        let rel = *off - self.ptr;
                        rel < eff.touched.start || rel >= eff.touched.end
                    });
                } else {
                    self.clobber_all();
                }
                self.cells.insert(self.ptr, CellLattice::Zero);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_program_knows_origin_cell_only() {
        let st = TapeState::new_program();
        assert_eq!(st.value_at(0), Some(0));
        assert_eq!(st.value_at(1), None);
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        assert_eq!(st.lattice_at(1), CellLattice::Top);
    }

    #[test]
    fn new_block_is_empty() {
        let st = TapeState::new_block();
        assert_eq!(st.value_at(0), None);
        assert_eq!(st.lattice_at(0), CellLattice::Top);
    }

    #[test]
    fn add_on_unknown_cell_stays_unknown() {
        // Literal-equivalence with the old ConstEnv: Add on ⊤ must NOT invent
        // any fact, even NonZero.
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Add(5));
        assert_eq!(st.value_at_ptr(), None);
        assert_eq!(st.lattice_at(0), CellLattice::Top);
    }

    #[test]
    fn zero_then_add_is_known_constant() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(7));
        assert_eq!(st.value_at_ptr(), Some(7));
        assert_eq!(st.lattice_at(0), CellLattice::Const(7));
    }

    #[test]
    fn zero_wraps_add_mod_256() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(-1));
        assert_eq!(st.value_at_ptr(), Some(255));
        assert_eq!(st.lattice_at(0), CellLattice::Const(255));
        st.apply(&HirInst::Add(2));
        assert_eq!(st.value_at_ptr(), Some(1));
        assert_eq!(st.lattice_at(0), CellLattice::Const(1));
    }

    #[test]
    fn add_that_wraps_to_zero_normalises_to_zero_lattice() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(5));
        st.apply(&HirInst::Add(-5));
        assert_eq!(st.value_at_ptr(), Some(0));
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        assert!(st.is_known_zero_at_ptr());
    }

    #[test]
    fn move_then_zero_records_remote_cell() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Move(3));
        st.apply(&HirInst::Zero);
        assert_eq!(st.value_at(3), Some(0));
        assert_eq!(st.value_at(0), None);
        assert_eq!(st.value_at_ptr(), Some(0));
        assert_eq!(st.lattice_at(3), CellLattice::Zero);
    }

    #[test]
    fn get_byte_invalidates_only_current_cell() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Move(1));
        st.apply(&HirInst::Zero);
        assert_eq!(st.value_at(0), Some(0));
        assert_eq!(st.value_at(1), Some(0));
        st.apply(&HirInst::GetByte);
        assert_eq!(st.value_at(1), None);
        assert_eq!(st.lattice_at(1), CellLattice::Top);
        assert_eq!(st.value_at(0), Some(0));
    }

    #[test]
    fn put_byte_preserves_all_facts() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::PutByte);
        assert_eq!(st.value_at(0), Some(0));
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
    }

    #[test]
    fn loop_preserves_untouched_cells_and_zeroes_head() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Add(5)); // cell[0] = 5
        st.apply(&HirInst::Move(5));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(42)); // cell[5] = 42
        st.apply(&HirInst::Move(-5)); // ptr = 0
        // Balanced loop body: Add(-1), Move(1), Add(1), Move(-1) — touches 0..2
        st.apply(&HirInst::Loop(vec![
            HirInst::Add(-1),
            HirInst::Move(1),
            HirInst::Add(1),
            HirInst::Move(-1),
        ]));
        // Head cell is zero after loop exit
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        // cell[1] is in the touched range (0..2), so clobbered
        assert_eq!(st.lattice_at(1), CellLattice::Top);
        // cell[5] is outside the touched range, preserved
        assert_eq!(st.value_at(5), Some(42));
    }

    #[test]
    fn loop_unbalanced_clobbers_all_but_zeroes_head() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Add(5));
        st.apply(&HirInst::Move(3));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(99)); // cell[3] = 99
        st.apply(&HirInst::Move(-3));
        // Unbalanced loop: body has net_ptr_delta != 0
        st.apply(&HirInst::Loop(vec![HirInst::Move(1)]));
        // Head cell is still zero after loop exit
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        // Everything else is clobbered
        assert_eq!(st.value_at(3), None);
    }

    #[test]
    fn empty_loop_preserves_all_facts_and_zeroes_head() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Loop(vec![]));
        // Empty loop body touches nothing; head is zero after exit
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
    }

    #[test]
    fn scan_clobbers_all_cells() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Scan(1));
        assert_eq!(st.value_at(0), None);
    }

    #[test]
    fn linear_mul_zeroes_head_and_preserves_remote() {
        let mut st = TapeState::new_program();
        // Set cell[0] = 0 (already), cell[5] = 42 via Zero+Add
        st.apply(&HirInst::Move(5));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(42));
        st.apply(&HirInst::Move(-5));
        // LinearMul at ptr=0 with factors [(1,1), (2,3)]
        st.apply(&HirInst::LinearMul(vec![(1, 1), (2, 3)]));
        // Head cell is always zeroed
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        // cell[5] is untouched by LinearMul (not in factors)
        assert_eq!(st.value_at(5), Some(42));
    }

    #[test]
    fn linear_mul_known_head_computes_factor_targets() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(3)); // cell[0] = 3
        st.apply(&HirInst::Move(1));
        st.apply(&HirInst::Zero); // cell[1] = 0
        st.apply(&HirInst::Move(1));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(10)); // cell[2] = 10
        st.apply(&HirInst::Move(-2)); // ptr = 0
        // LinearMul: v=3, *p=0, cell[1] += 3*1=3, cell[2] += 3*2=6
        st.apply(&HirInst::LinearMul(vec![(1, 1), (2, 2)]));
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        assert_eq!(st.value_at(1), Some(3)); // 0 + 3*1
        assert_eq!(st.value_at(2), Some(16)); // 10 + 3*2
    }

    #[test]
    fn linear_mul_unknown_head_clobbers_factor_targets_only() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::GetByte); // cell[0] = Top
        st.apply(&HirInst::Move(2));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(99)); // cell[2] = 99
        st.apply(&HirInst::Move(-2)); // ptr = 0
        st.apply(&HirInst::LinearMul(vec![(1, 1)]));
        assert_eq!(st.lattice_at(0), CellLattice::Zero); // head always zeroed
        assert_eq!(st.lattice_at(1), CellLattice::Top); // factor target clobbered
        assert_eq!(st.value_at(2), Some(99)); // untouched
    }

    #[test]
    fn linear_mul_with_sets_zero_head_is_noop() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero); // cell[0] = 0
        st.apply(&HirInst::Move(1));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(77)); // cell[1] = 77
        st.apply(&HirInst::Move(-1));
        st.apply(&HirInst::LinearMulWithSets {
            factors: vec![(1, 1)],
            sets: vec![2],
        });
        // v == 0 → no-op, all facts preserved
        assert_eq!(st.lattice_at(0), CellLattice::Zero);
        assert_eq!(st.value_at(1), Some(77));
    }

    #[test]
    fn linear_mul_with_sets_known_nonzero_head() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(5)); // cell[0] = 5
        st.apply(&HirInst::Move(1));
        st.apply(&HirInst::Zero); // cell[1] = 0
        st.apply(&HirInst::Move(-1));
        st.apply(&HirInst::LinearMulWithSets {
            factors: vec![(1, 2)],
            sets: vec![3],
        });
        assert_eq!(st.lattice_at(0), CellLattice::Zero); // head zeroed
        assert_eq!(st.value_at(1), Some(10)); // 0 + 5*2
        assert_eq!(st.lattice_at(3), CellLattice::Zero); // set target zeroed
    }

    #[test]
    fn linear_mul_with_sets_top_head_clobbers_sets() {
        let mut st = TapeState::new_block();
        st.apply(&HirInst::GetByte); // cell[0] = Top
        st.apply(&HirInst::Move(2));
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Add(50)); // cell[2] = 50
        st.apply(&HirInst::Move(-2));
        st.apply(&HirInst::LinearMulWithSets {
            factors: vec![(1, 1)],
            sets: vec![3],
        });
        // head is Top (not provably zero), so instruction may or may not fire
        assert_eq!(st.lattice_at(0), CellLattice::Zero); // head zeroed either way
        assert_eq!(st.lattice_at(1), CellLattice::Top); // factor target clobbered
        assert_eq!(st.lattice_at(3), CellLattice::Top); // set target uncertain
        assert_eq!(st.value_at(2), Some(50)); // untouched
    }

    #[test]
    fn merge_same_ptr_same_fact_preserves_fact() {
        let mut a = TapeState::new_program();
        let b = TapeState::new_program();
        a.merge_in_place(&b);
        assert!(!a.is_pessimistic());
        assert_eq!(a.lattice_at(0), CellLattice::Zero);
    }

    #[test]
    fn merge_different_ptr_becomes_pessimistic() {
        let mut a = TapeState::new_program();
        let mut b = TapeState::new_program();
        b.apply(&HirInst::Move(1));
        a.merge_in_place(&b);
        assert!(a.is_pessimistic());
        // Pessimistic state returns Top for every offset.
        assert_eq!(a.lattice_at(0), CellLattice::Top);
        assert_eq!(a.lattice_at(1), CellLattice::Top);
    }

    #[test]
    fn merge_conflicting_cell_facts_joins_to_top() {
        let mut a = TapeState::new_block();
        a.apply(&HirInst::Zero);
        a.apply(&HirInst::Add(3));
        let mut b = TapeState::new_block();
        b.apply(&HirInst::Zero);
        b.apply(&HirInst::Add(5));
        a.merge_in_place(&b);
        assert!(!a.is_pessimistic());
        // Const(3) meet Const(5) = NonZero.
        assert_eq!(a.lattice_at(0), CellLattice::NonZero);
    }

    #[test]
    fn merge_with_pessimistic_becomes_pessimistic() {
        let mut a = TapeState::new_program();
        let mut b = TapeState::new_program();
        let mut extra = TapeState::new_program();
        extra.apply(&HirInst::Move(1));
        b.merge_in_place(&extra);
        assert!(b.is_pessimistic());
        a.merge_in_place(&b);
        assert!(a.is_pessimistic());
    }

    #[test]
    fn apply_on_pessimistic_is_noop() {
        let mut a = TapeState::new_program();
        let mut extra = TapeState::new_program();
        extra.apply(&HirInst::Move(1));
        a.merge_in_place(&extra);
        assert!(a.is_pessimistic());
        a.apply(&HirInst::Zero);
        a.apply(&HirInst::Add(5));
        assert!(a.is_pessimistic());
        assert_eq!(a.lattice_at(0), CellLattice::Top);
    }

    #[test]
    fn add_on_nonzero_full_period_preserves_nonzero() {
        // Construct a NonZero cell via a merge of two distinct non-zero consts,
        // then verify that Add(256) keeps it NonZero (full-period shift).
        let mut a = TapeState::new_block();
        a.apply(&HirInst::Zero);
        a.apply(&HirInst::Add(3));
        let mut b = TapeState::new_block();
        b.apply(&HirInst::Zero);
        b.apply(&HirInst::Add(5));
        a.merge_in_place(&b);
        assert_eq!(a.lattice_at(0), CellLattice::NonZero);

        a.apply(&HirInst::Add(256));
        assert_eq!(a.lattice_at(0), CellLattice::NonZero);
    }

    #[test]
    fn add_on_nonzero_partial_shift_retreats_to_top() {
        // Same NonZero construction, then Add(1) which could cross zero.
        let mut a = TapeState::new_block();
        a.apply(&HirInst::Zero);
        a.apply(&HirInst::Add(3));
        let mut b = TapeState::new_block();
        b.apply(&HirInst::Zero);
        b.apply(&HirInst::Add(5));
        a.merge_in_place(&b);
        assert_eq!(a.lattice_at(0), CellLattice::NonZero);

        a.apply(&HirInst::Add(1));
        assert_eq!(a.lattice_at(0), CellLattice::Top);
        // The sparse map must not retain a stale NonZero entry after the
        // retreat; absence == Top by convention.
        assert_eq!(a.value_at_ptr(), None);
    }

    #[test]
    fn is_known_zero_at_ptr_matches_zero_lattice() {
        let mut st = TapeState::new_block();
        assert!(!st.is_known_zero_at_ptr());
        st.apply(&HirInst::Zero);
        assert!(st.is_known_zero_at_ptr());
        st.apply(&HirInst::Add(1));
        assert!(!st.is_known_zero_at_ptr());
        assert_eq!(st.lattice_at(0), CellLattice::Const(1));
    }
}
