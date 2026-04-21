//! Per-block symbolic tape state.
//!
//! Tracks the data pointer offset (relative to the block entry) and a sparse
//! map of cell facts expressed in the [`CellLattice`] four-point domain.
//! `apply` transfers a single [`HirInst`] with the same semantics previously
//! inlined as `ConstEnv` in `ir::optimize`.
//!
//! Migration note (A3b): cells now carry `CellLattice` instead of the
//! `Option<u8>` placeholder, but the transfer function is deliberately
//! conservative — `Add(k)` on `Top` stays `Top` (no promotion to `NonZero`
//! or `Const(_)`). This keeps the fact set byte-for-byte equivalent to the
//! old `ConstEnv`, so existing `O1`/`O2` consumers observe no IR drift.
//! Later passes will enrich the transfer to exploit `NonZero`/`Const` facts.
//!
//! Absence of a key in [`TapeState::cells`] represents `Top` (unknown).

use std::collections::BTreeMap;

use crate::ir::analysis::lattice::CellLattice;
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
    /// unanalysed control structures (`Loop` / `Scan` / `LinearMul`) render
    /// prior cell facts unreliable.
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
                // Literal-equivalence mode: only refine when the cell is
                // already a known constant (Zero or Const(_)); otherwise
                // leave Top/NonZero untouched. A later pass will tighten
                // the transfer to exploit non-zero / zero lattice points.
                if let Some(v) = self.lattice_at(self.ptr).known_u8() {
                    let next = (v as i32 + *k).rem_euclid(256) as u8;
                    self.cells.insert(self.ptr, CellLattice::set_const(next));
                }
            }
            HirInst::Zero => {
                self.cells.insert(self.ptr, CellLattice::Zero);
            }
            HirInst::GetByte => {
                self.cells.remove(&self.ptr);
            }
            HirInst::PutByte => { /* pure side effect on stdout */ }
            HirInst::LinearMul(_) | HirInst::Scan(_) | HirInst::Loop(_) => {
                self.clobber_all();
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
    fn loop_clobbers_all_cells() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Loop(vec![]));
        assert_eq!(st.value_at(0), None);
        assert_eq!(st.lattice_at(0), CellLattice::Top);
    }

    #[test]
    fn scan_clobbers_all_cells() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::Scan(1));
        assert_eq!(st.value_at(0), None);
    }

    #[test]
    fn linear_mul_clobbers_all_cells() {
        let mut st = TapeState::new_program();
        st.apply(&HirInst::Zero);
        st.apply(&HirInst::LinearMul(vec![(1, 1)]));
        assert_eq!(st.value_at(0), None);
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
