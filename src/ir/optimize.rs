use std::collections::BTreeMap;

use crate::ir::hir::{HirInst, HirProgram};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum OptimizeError {
    #[error("optimization did not converge within {max_iters} iterations")]
    DidNotConverge { max_iters: usize },
}

/// Baseline HIR cleanup: fuse consecutive `Add` / `Move` (single forward pass per block).
pub(crate) fn optimize_o0(program: HirProgram) -> HirProgram {
    HirProgram {
        insts: optimize_block_o0(program.insts),
    }
}

/// `-O1`: single pass over each block — fusion, affine / scan / clear loops, peephole on `Zero`,
/// constant propagation for dead empty-loop removal, and local `Zero`/`Add` simplification.
pub(crate) fn optimize_o1(program: HirProgram) -> HirProgram {
    HirProgram {
        insts: optimize_block_o1(program.insts),
    }
}

/// `-O2`: repeat the `-O1` pipeline until the HIR reaches a fixed point (no further changes).
#[cfg(test)]
pub(crate) fn optimize_o2(program: HirProgram) -> HirProgram {
    try_optimize_o2(program).expect("optimize_o2: fixed-point optimization should converge")
}

pub(crate) fn try_optimize_o2(program: HirProgram) -> Result<HirProgram, OptimizeError> {
    const MAX_ITERS: usize = 4096;
    let mut current = program;
    for _ in 0..MAX_ITERS {
        let next = optimize_o1(current.clone());
        if next == current {
            return Ok(next);
        }
        current = next;
    }
    Err(OptimizeError::DidNotConverge {
        max_iters: MAX_ITERS,
    })
}

fn optimize_block_o0(insts: Vec<HirInst>) -> Vec<HirInst> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < insts.len() {
        match &insts[i] {
            HirInst::Add(v) => {
                let mut total = *v;
                i += 1;

                while i < insts.len() {
                    if let HirInst::Add(v2) = &insts[i] {
                        total += *v2;
                        i += 1;
                    } else {
                        break;
                    }
                }

                if total != 0 {
                    out.push(HirInst::Add(total));
                }
            }

            HirInst::Move(v) => {
                let mut total = *v;
                i += 1;

                while i < insts.len() {
                    if let HirInst::Move(v2) = &insts[i] {
                        total += *v2;
                        i += 1;
                    } else {
                        break;
                    }
                }

                if total != 0 {
                    out.push(HirInst::Move(total));
                }
            }

            HirInst::Loop(body) => {
                out.push(HirInst::Loop(optimize_block_o0(body.clone())));
                i += 1;
            }

            other => {
                out.push(other.clone());
                i += 1;
            }
        }
    }

    out
}

fn fuse_add_move(insts: Vec<HirInst>) -> Vec<HirInst> {
    optimize_block_o0(insts)
}

/// Affine simple loop: only `Move`/`Add`, net pointer delta 0, head cell `-1` per iteration (mod 256).
fn try_linear_loop(body: &[HirInst]) -> Option<Vec<(isize, i32)>> {
    let mut ptr: isize = 0;
    let mut delta: BTreeMap<isize, i32> = BTreeMap::new();

    for inst in body {
        match inst {
            HirInst::Move(d) => ptr += *d,
            HirInst::Add(k) => {
                *delta.entry(ptr).or_insert(0) += k;
            }
            HirInst::Loop(_)
            | HirInst::PutByte
            | HirInst::GetByte
            | HirInst::Zero
            | HirInst::LinearMul(_)
            | HirInst::Scan(_) => return None,
        }
    }

    if ptr != 0 {
        return None;
    }

    let d0 = *delta.get(&0).unwrap_or(&0);
    if d0.rem_euclid(256) != 255 {
        return None;
    }

    let mut factors: Vec<(isize, i32)> = delta.into_iter().filter(|(off, _)| *off != 0).collect();
    factors.sort_by_key(|(o, _)| *o);
    Some(factors)
}

fn try_scan_loop(body: &[HirInst]) -> Option<isize> {
    match body {
        [HirInst::Move(d)] if *d == 1 || *d == -1 => Some(*d),
        _ => None,
    }
}

/// `[-]` on an 8-bit cell: body is a single wrapped decrement-by-one per iteration.
fn is_byte_clear_loop(body: &[HirInst]) -> bool {
    match body {
        [HirInst::Add(k)] => k.rem_euclid(256) == 255,
        _ => false,
    }
}

fn try_loop_specialize(inner: &[HirInst]) -> Option<HirInst> {
    if let Some(dir) = try_scan_loop(inner) {
        return Some(HirInst::Scan(dir));
    }
    if is_byte_clear_loop(inner) {
        return Some(HirInst::Zero);
    }
    if let Some(factors) = try_linear_loop(inner) {
        if factors.is_empty() {
            return Some(HirInst::Zero);
        }
        return Some(HirInst::LinearMul(factors));
    }
    None
}

/// Known BF tape cells (byte) at absolute indices; missing ⇒ unknown (not assumed 0).
struct ConstEnv {
    ptr: isize,
    known: BTreeMap<isize, u8>,
}

impl ConstEnv {
    fn new_block() -> Self {
        Self {
            ptr: 0,
            known: BTreeMap::new(),
        }
    }

    /// Whole-program entry: tape all zeros.
    fn new_program() -> Self {
        let mut k = BTreeMap::new();
        k.insert(0, 0);
        Self { ptr: 0, known: k }
    }

    fn value_at_ptr(&self) -> Option<u8> {
        self.known.get(&self.ptr).copied()
    }

    fn after_control_flow(&mut self) {
        self.known.clear();
    }

    fn apply_inst(&mut self, inst: &HirInst) {
        match inst {
            HirInst::Move(d) => self.ptr += *d,
            HirInst::Add(k) => {
                if let Some(v) = self.known.get(&self.ptr).copied() {
                    let next = (v as i32 + k).rem_euclid(256) as u8;
                    self.known.insert(self.ptr, next);
                }
            }
            HirInst::Zero => {
                self.known.insert(self.ptr, 0);
            }
            HirInst::LinearMul(_) | HirInst::Scan(_) => {
                self.after_control_flow();
            }
            HirInst::GetByte => {
                self.known.remove(&self.ptr);
            }
            HirInst::PutByte => {}
            HirInst::Loop(_) => {
                self.after_control_flow();
            }
        }
    }
}

fn optimize_block_o1(insts: Vec<HirInst>) -> Vec<HirInst> {
    optimize_block_o1_with_parent_env(insts, false)
}

fn optimize_block_o1_with_parent_env(insts: Vec<HirInst>, nested: bool) -> Vec<HirInst> {
    let insts = fuse_add_move(insts);
    let mut env = if nested {
        ConstEnv::new_block()
    } else {
        ConstEnv::new_program()
    };

    let mut out = Vec::new();
    let mut i = 0;

    while i < insts.len() {
        match &insts[i] {
            HirInst::Add(v) => {
                let mut total = *v;
                i += 1;

                while i < insts.len() {
                    if let HirInst::Add(v2) = &insts[i] {
                        total += *v2;
                        i += 1;
                    } else {
                        break;
                    }
                }

                if total != 0 {
                    let inst = HirInst::Add(total);
                    env.apply_inst(&inst);
                    push_o1(&mut out, inst);
                }
            }

            HirInst::Move(v) => {
                let mut total = *v;
                i += 1;

                while i < insts.len() {
                    if let HirInst::Move(v2) = &insts[i] {
                        total += *v2;
                        i += 1;
                    } else {
                        break;
                    }
                }

                if total != 0 {
                    let inst = HirInst::Move(total);
                    env.apply_inst(&inst);
                    push_o1(&mut out, inst);
                }
            }

            HirInst::Loop(body) => {
                let inner = optimize_block_o1_with_parent_env(body.clone(), true);

                if inner.is_empty() {
                    if env.value_at_ptr() == Some(0) {
                        i += 1;
                        continue;
                    }
                    push_o1(&mut out, HirInst::Loop(inner));
                    env.after_control_flow();
                    i += 1;
                    continue;
                }

                if let Some(spec) = try_loop_specialize(&inner) {
                    env.apply_inst(&spec);
                    push_o1(&mut out, spec);
                } else {
                    push_o1(&mut out, HirInst::Loop(inner));
                    env.after_control_flow();
                }
                i += 1;
            }

            other => {
                let inst = other.clone();
                env.apply_inst(&inst);
                push_o1(&mut out, inst);
                i += 1;
            }
        }
    }

    out
}

fn push_o1(out: &mut Vec<HirInst>, inst: HirInst) {
    match inst {
        HirInst::Add(0) => {}

        HirInst::Add(k) => {
            out.push(HirInst::Add(k));
        }

        HirInst::Zero => {
            if matches!(out.last(), Some(HirInst::Zero)) {
                return;
            }
            if matches!(out.last(), Some(HirInst::Add(_))) {
                out.pop();
            }
            out.push(HirInst::Zero);
        }

        other => out.push(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o1_clears_simple_minus_loop() {
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Add(-1)])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::Zero]);
    }

    #[test]
    fn o1_zero_then_add_kept() {
        // `Zero; Add(k)` is *not* `Add(k)`: the former clears then adds; the latter is relative to the prior value.
        let p = HirProgram {
            insts: vec![HirInst::Zero, HirInst::Add(7)],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::Zero, HirInst::Add(7)]);
    }

    #[test]
    fn o1_add_then_zero_collapses() {
        let p = HirProgram {
            insts: vec![HirInst::Add(3), HirInst::Zero],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::Zero]);
    }

    #[test]
    fn o1_copy_loop_to_linear_mul() {
        // [->+<]
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Add(-1),
                HirInst::Move(1),
                HirInst::Add(1),
                HirInst::Move(-1),
            ])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::LinearMul(vec![(1, 1)])]);
    }

    #[test]
    fn o1_scan_right() {
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Move(1)])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::Scan(1)]);
    }

    #[test]
    fn o1_dead_empty_loop_removed() {
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![])],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty());
    }

    #[test]
    fn o2_matches_o1_at_fixed_point() {
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Add(-1)])],
        };
        let once = optimize_o1(p.clone());
        let twice = optimize_o2(p);
        assert_eq!(once, twice);
    }
}
