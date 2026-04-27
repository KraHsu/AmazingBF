//! Dead store elimination over HIR (pass B1).
//!
//! Rewrites each block with a forward peephole: when a write at a given
//! tape offset is overwritten by a later write to the same offset without
//! any intervening read, the earlier write is dropped. Covers the cases
//! that [`super::optimize::push_o1`] misses — in particular `Add` / `Zero`
//! at the same offset separated by cancelling `Move` pairs, or an
//! `Add` / `Zero` followed by a `GetByte` (which overwrites without
//! reading).
//!
//! The pass is deliberately syntactic: it tracks only a virtual pointer
//! offset from the block entry and a sparse "pending writes" map. It does
//! **not** consult [`crate::ir::analysis::TapeState`] — the shadow decision
//! is independent of cell-value lattice facts. A3's richer transfer
//! remains relevant to other Phase B consumers (B2 / B5) but DSE does not
//! need it.
//!
//! `Loop` / `Scan` / `LinearMul` act as barriers (unknown reads/writes);
//! `Loop` bodies are recursed into. `PutByte` reads the current cell;
//! `Add` reads-then-writes. `GetByte` writes without reading and is never
//! itself dead (input side effect).

use std::collections::BTreeMap;

use crate::ir::hir::{HirInst, HirProgram};

/// Apply DSE to every block in `program`, returning a new program.
pub(crate) fn dead_store_elimination(program: HirProgram) -> HirProgram {
    HirProgram {
        insts: dse_block(program.insts),
    }
}

fn dse_block(insts: Vec<HirInst>) -> Vec<HirInst> {
    let mut out: Vec<Option<HirInst>> = Vec::with_capacity(insts.len());
    let mut pending: BTreeMap<isize, usize> = BTreeMap::new();
    let mut virt_ptr: isize = 0;

    for inst in insts {
        match inst {
            HirInst::Move(d) => {
                virt_ptr += d;
                out.push(Some(HirInst::Move(d)));
            }
            HirInst::Add(k) => {
                // Add reads-then-writes: commit any prior pending at this
                // offset (the prior write is observed by the Add), then
                // register the Add itself as overwrite-killable by a later
                // Zero / GetByte.
                pending.remove(&virt_ptr);
                let idx = out.len();
                out.push(Some(HirInst::Add(k)));
                pending.insert(virt_ptr, idx);
            }
            HirInst::Zero => {
                // Unconditional write: any prior pending at this offset is
                // shadowed and can be dropped.
                if let Some(idx) = pending.remove(&virt_ptr) {
                    out[idx] = None;
                }
                let idx = out.len();
                out.push(Some(HirInst::Zero));
                pending.insert(virt_ptr, idx);
            }
            HirInst::GetByte => {
                // Input overwrites the cell without reading it, so a prior
                // pending write here is dead. GetByte itself is not
                // registered — its input side effect means it is never
                // dead, and we must never drop it.
                if let Some(idx) = pending.remove(&virt_ptr) {
                    out[idx] = None;
                }
                out.push(Some(HirInst::GetByte));
            }
            HirInst::PutByte => {
                // Output reads the current cell: commit (do not drop)
                // prior pending at this offset.
                pending.remove(&virt_ptr);
                out.push(Some(HirInst::PutByte));
            }
            HirInst::Loop(body) => {
                let new_body = dse_block(body);
                pending.clear();
                out.push(Some(HirInst::Loop(new_body)));
            }
            HirInst::Scan(d) => {
                pending.clear();
                out.push(Some(HirInst::Scan(d)));
            }
            HirInst::LinearMul(factors) => {
                pending.clear();
                out.push(Some(HirInst::LinearMul(factors)));
            }
            HirInst::LinearMulWithSets { factors, sets } => {
                pending.clear();
                out.push(Some(HirInst::LinearMulWithSets { factors, sets }));
            }
        }
    }

    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dse(insts: Vec<HirInst>) -> Vec<HirInst> {
        dead_store_elimination(HirProgram { insts }).insts
    }

    #[test]
    fn shadowed_zero_at_same_offset_removed() {
        // Baseline case already handled by push_o1 at the O1 level; DSE
        // should produce the same result in isolation.
        let input = vec![HirInst::Add(3), HirInst::Zero];
        assert_eq!(dse(input), vec![HirInst::Zero]);
    }

    #[test]
    fn zero_shadowed_across_move_pair_removed() {
        // The prior Zero at off 0 is dead: the trailing Zero at off 0
        // overwrites it, and the interleaved Add is at off 2 (no read at
        // off 0 between the two Zeros).
        let input = vec![
            HirInst::Zero,
            HirInst::Move(2),
            HirInst::Add(1),
            HirInst::Move(-2),
            HirInst::Zero,
        ];
        assert_eq!(
            dse(input),
            vec![
                HirInst::Move(2),
                HirInst::Add(1),
                HirInst::Move(-2),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn put_byte_between_writes_blocks_dse() {
        // PutByte reads cell[0], so the first Zero is observed and kept.
        let input = vec![HirInst::Zero, HirInst::PutByte, HirInst::Zero];
        assert_eq!(
            dse(input.clone()),
            vec![HirInst::Zero, HirInst::PutByte, HirInst::Zero]
        );
    }

    #[test]
    fn loop_between_writes_blocks_dse() {
        // Loop may read any cell, so pending writes are committed.
        let input = vec![
            HirInst::Zero,
            HirInst::Loop(vec![HirInst::Add(1)]),
            HirInst::Zero,
        ];
        assert_eq!(
            dse(input.clone()),
            vec![
                HirInst::Zero,
                HirInst::Loop(vec![HirInst::Add(1)]),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn get_byte_shadows_prior_store() {
        // GetByte overwrites cell[0] without reading it, so the Add(5) is
        // dead. GetByte itself is preserved (input side effect).
        let input = vec![HirInst::Add(5), HirInst::GetByte];
        assert_eq!(dse(input), vec![HirInst::GetByte]);
    }

    #[test]
    fn recursive_into_loop_body() {
        // DSE must recurse into Loop bodies.
        let input = vec![HirInst::Loop(vec![HirInst::Zero, HirInst::Zero])];
        assert_eq!(dse(input), vec![HirInst::Loop(vec![HirInst::Zero])]);
    }

    #[test]
    fn add_then_zero_across_move_pair_kills_add() {
        // Cross-move variant of the push_o1 case: Add@0, Move(+1), Move(-1),
        // Zero@0 → the Add is dead even though push_o1 wouldn't see the
        // adjacency. (After O1 fusion the Moves cancel, but this test drives
        // the raw DSE invariant directly.)
        let input = vec![
            HirInst::Add(7),
            HirInst::Move(1),
            HirInst::Move(-1),
            HirInst::Zero,
        ];
        assert_eq!(
            dse(input),
            vec![HirInst::Move(1), HirInst::Move(-1), HirInst::Zero]
        );
    }

    #[test]
    fn scan_acts_as_barrier() {
        let input = vec![HirInst::Zero, HirInst::Scan(1), HirInst::Zero];
        assert_eq!(
            dse(input),
            vec![HirInst::Zero, HirInst::Scan(1), HirInst::Zero]
        );
    }

    #[test]
    fn linear_mul_acts_as_barrier() {
        let input = vec![
            HirInst::Zero,
            HirInst::LinearMul(vec![(1, 1)]),
            HirInst::Zero,
        ];
        assert_eq!(
            dse(input),
            vec![
                HirInst::Zero,
                HirInst::LinearMul(vec![(1, 1)]),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn preserves_programs_with_no_dead_stores() {
        // Realistic "print then read then print" snippet where every
        // write is observed by a read before the next overwrite — DSE
        // must be a no-op.
        let input = vec![
            HirInst::Zero,
            HirInst::Add(65),
            HirInst::PutByte,
            HirInst::GetByte,
            HirInst::PutByte,
        ];
        assert_eq!(dse(input.clone()), input);
    }

    #[test]
    fn zero_shadowed_by_get_byte_removed() {
        // GetByte overwrites cell[0] without reading, so the preceding
        // Zero is dead even though it was the active pending write.
        let input = vec![HirInst::Zero, HirInst::GetByte];
        assert_eq!(dse(input), vec![HirInst::GetByte]);
    }

    #[test]
    fn empty_block_is_identity() {
        let input: Vec<HirInst> = vec![];
        assert_eq!(dse(input), Vec::<HirInst>::new());
    }
}
