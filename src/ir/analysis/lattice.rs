//! Four-point abstract domain for a single tape cell.
//!
//! `Top` is "unknown"; `NonZero` and `Zero` split the value space by
//! loop-header relevance; `Const(v)` is a concrete byte. The lattice is
//! deliberately flat — no `Bottom` — because unreachable paths in the HIR
//! analyzer are expressed with `Option<TapeState>::None` at the fact level,
//! not with a per-cell ⊥.
//!
//! The lattice's height is 4, so any transfer function that only meets with
//! priors converges quickly (well under the `MAX_LOOP_ITERS` budget planned
//! for A4a's `run_forward`).
//!
//! Until A3b integrates this into `TapeState`, the module has no non-test
//! consumers — the module-level `allow(dead_code)` keeps the build clean.
//! Remove it when `TapeState` is refactored to carry `CellLattice` values.

#![cfg_attr(not(test), allow(dead_code))]

/// Abstract value of a single tape cell.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum CellLattice {
    /// Unknown: any byte value, or could be unreachable at this point.
    Top,
    /// Known non-zero, exact value not tracked.
    NonZero,
    /// Known zero.
    Zero,
    /// Known constant byte value. `Const(0)` is normalised to `Zero`;
    /// every other value is `Const(v)` with `v != 0`.
    Const(u8),
}

impl CellLattice {
    /// Lattice meet (⊓): least upper bound over {Top, NonZero, Zero, Const(v)}.
    ///
    /// Key points:
    /// - `Zero ⊓ NonZero = Top` (they overlap only in ⊥, which we don't model).
    /// - `Const(u) ⊓ Const(v)` with `u != v` joins upward to `NonZero` when
    ///   both are non-zero, otherwise `Top`.
    /// - `Top ⊓ x = Top` for all `x`.
    pub(crate) fn meet(self, other: Self) -> Self {
        use CellLattice::*;
        match (self, other) {
            (Top, _) | (_, Top) => Top,

            (Zero, Zero) => Zero,
            (Zero, NonZero) | (NonZero, Zero) => Top,
            (Zero, Const(v)) | (Const(v), Zero) => {
                if v == 0 {
                    Zero
                } else {
                    Top
                }
            }

            (NonZero, NonZero) => NonZero,
            (NonZero, Const(v)) | (Const(v), NonZero) => {
                if v == 0 {
                    Top
                } else {
                    NonZero
                }
            }

            (Const(u), Const(v)) => {
                if u == v {
                    Const(u)
                } else if u != 0 && v != 0 {
                    NonZero
                } else {
                    Top
                }
            }
        }
    }

    /// `true` iff the cell is provably zero.
    pub(crate) fn is_zero(self) -> bool {
        matches!(self, CellLattice::Zero | CellLattice::Const(0))
    }

    /// `true` iff the cell is provably non-zero.
    pub(crate) fn is_nonzero(self) -> bool {
        match self {
            CellLattice::NonZero => true,
            CellLattice::Const(v) => v != 0,
            _ => false,
        }
    }

    /// Concrete byte value, if known exactly.
    pub(crate) fn known_u8(self) -> Option<u8> {
        match self {
            CellLattice::Zero => Some(0),
            CellLattice::Const(v) => Some(v),
            _ => None,
        }
    }

    /// Transfer `Add(k)` on an 8-bit wrap-around cell.
    ///
    /// Semantics:
    /// - `Top` stays `Top`.
    /// - `Const(v)` wraps to `Const((v + k) mod 256)`, normalising 0 to `Zero`.
    /// - `Zero + 0 = Zero`; `Zero + k` (`k mod 256 != 0`) becomes `Const(k mod 256)`.
    /// - `NonZero + k`: only `k mod 256 == 0` preserves `NonZero`; any other
    ///   shift crosses zero non-trivially, so we retreat to `Top`.
    pub(crate) fn add_wrapping(self, k: i32) -> Self {
        use CellLattice::*;
        let km = k.rem_euclid(256) as u8;
        match self {
            Top => Top,
            Const(v) => {
                let next = ((v as i32 + k).rem_euclid(256)) as u8;
                if next == 0 { Zero } else { Const(next) }
            }
            Zero => {
                if km == 0 {
                    Zero
                } else {
                    Const(km)
                }
            }
            NonZero => {
                if km == 0 {
                    NonZero
                } else {
                    Top
                }
            }
        }
    }

    /// Lift a concrete byte into the lattice, normalising 0 → `Zero`.
    pub(crate) fn set_const(v: u8) -> Self {
        if v == 0 {
            CellLattice::Zero
        } else {
            CellLattice::Const(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CellLattice::*;
    use super::*;

    #[test]
    fn meet_top_absorbs_everything() {
        assert_eq!(Top.meet(Top), Top);
        assert_eq!(Top.meet(NonZero), Top);
        assert_eq!(Top.meet(Zero), Top);
        assert_eq!(Top.meet(Const(7)), Top);
        assert_eq!(NonZero.meet(Top), Top);
        assert_eq!(Zero.meet(Top), Top);
        assert_eq!(Const(7).meet(Top), Top);
    }

    #[test]
    fn meet_zero_and_nonzero_is_top() {
        assert_eq!(Zero.meet(NonZero), Top);
        assert_eq!(NonZero.meet(Zero), Top);
    }

    #[test]
    fn meet_zero_pair() {
        assert_eq!(Zero.meet(Zero), Zero);
    }

    #[test]
    fn meet_nonzero_pair() {
        assert_eq!(NonZero.meet(NonZero), NonZero);
    }

    #[test]
    fn meet_zero_and_const_zero_is_zero() {
        assert_eq!(Zero.meet(Const(0)), Zero);
        assert_eq!(Const(0).meet(Zero), Zero);
    }

    #[test]
    fn meet_zero_and_const_nonzero_is_top() {
        assert_eq!(Zero.meet(Const(5)), Top);
        assert_eq!(Const(5).meet(Zero), Top);
    }

    #[test]
    fn meet_nonzero_and_const_nonzero_stays_nonzero() {
        assert_eq!(NonZero.meet(Const(5)), NonZero);
        assert_eq!(Const(5).meet(NonZero), NonZero);
    }

    #[test]
    fn meet_nonzero_and_const_zero_is_top() {
        assert_eq!(NonZero.meet(Const(0)), Top);
        assert_eq!(Const(0).meet(NonZero), Top);
    }

    #[test]
    fn meet_equal_consts() {
        assert_eq!(Const(42).meet(Const(42)), Const(42));
        assert_eq!(Const(0).meet(Const(0)), Const(0));
    }

    #[test]
    fn meet_distinct_nonzero_consts_become_nonzero() {
        assert_eq!(Const(3).meet(Const(5)), NonZero);
        assert_eq!(Const(1).meet(Const(255)), NonZero);
    }

    #[test]
    fn meet_zero_const_and_nonzero_const_is_top() {
        assert_eq!(Const(0).meet(Const(5)), Top);
        assert_eq!(Const(5).meet(Const(0)), Top);
    }

    #[test]
    fn is_zero_and_is_nonzero_partition_concrete_values() {
        assert!(Zero.is_zero());
        assert!(Const(0).is_zero());
        assert!(!NonZero.is_zero());
        assert!(!Const(5).is_zero());
        assert!(!Top.is_zero());

        assert!(NonZero.is_nonzero());
        assert!(Const(5).is_nonzero());
        assert!(!Zero.is_nonzero());
        assert!(!Const(0).is_nonzero());
        assert!(!Top.is_nonzero());
    }

    #[test]
    fn known_u8_projects_concrete_values() {
        assert_eq!(Zero.known_u8(), Some(0));
        assert_eq!(Const(7).known_u8(), Some(7));
        assert_eq!(NonZero.known_u8(), None);
        assert_eq!(Top.known_u8(), None);
    }

    #[test]
    fn add_wrapping_on_const_wraps_mod_256() {
        assert_eq!(Const(200).add_wrapping(100), Const(44));
        assert_eq!(Const(10).add_wrapping(-15), Const(251));
        // Wrap-to-zero normalises to Zero.
        assert_eq!(Const(1).add_wrapping(-1), Zero);
        assert_eq!(Const(128).add_wrapping(128), Zero);
    }

    #[test]
    fn add_wrapping_on_top_stays_top() {
        assert_eq!(Top.add_wrapping(0), Top);
        assert_eq!(Top.add_wrapping(3), Top);
    }

    #[test]
    fn add_wrapping_on_zero() {
        assert_eq!(Zero.add_wrapping(0), Zero);
        assert_eq!(Zero.add_wrapping(7), Const(7));
        // k mod 256 == 0 with non-zero k still preserves Zero.
        assert_eq!(Zero.add_wrapping(256), Zero);
        assert_eq!(Zero.add_wrapping(-256), Zero);
    }

    #[test]
    fn add_wrapping_on_nonzero() {
        assert_eq!(NonZero.add_wrapping(3), Top);
        // Full-period shifts preserve NonZero.
        assert_eq!(NonZero.add_wrapping(256), NonZero);
        assert_eq!(NonZero.add_wrapping(-256), NonZero);
        assert_eq!(NonZero.add_wrapping(0), NonZero);
    }

    #[test]
    fn set_const_normalises_zero() {
        assert_eq!(CellLattice::set_const(0), Zero);
        assert_eq!(CellLattice::set_const(1), Const(1));
        assert_eq!(CellLattice::set_const(255), Const(255));
    }
}
