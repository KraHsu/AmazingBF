//! HIR optimization passes (O0 / O1 / O2 / O3).
//!
//! O0 fuses consecutive `Move` / `Add`; O1 adds pattern recognition
//! (`[-]` → `Zero`, `[>]` / `[<]` → `Scan`, simple affine loops →
//! `LinearMul`); O2 iterates O1 to a fixed point; O3 additionally permits
//! whole-program compile-time folds (only safe when the program reads no
//! input). Every pass is driven from the entry point
//! `optimize_program_for_opt_level`.

use std::collections::BTreeMap;

use crate::ir::analysis::dataflow::Transfer;
use crate::ir::analysis::tape_state::TapeState;
use crate::ir::dse::dead_store_elimination;
use crate::ir::hir::{HirInst, HirProgram};

/// Constant-propagation transfer function for the O1 rewrite pass. Thin
/// wrapper over [`TapeState::apply`] so the pass drives its symbolic
/// execution through the shared analysis API rather than calling `apply`
/// directly.
struct ConstPropXfer;

impl Transfer<TapeState> for ConstPropXfer {
    fn transfer_inst(&self, fact: &mut TapeState, inst: &HirInst) {
        fact.apply(inst);
    }
}

/// Errors produced by the optimization pipeline.
#[derive(Debug)]
pub enum OptimizeError {
    /// `-O2` / `-O3` fixed-point iteration exceeded its iteration budget.
    DidNotConverge {
        /// Iteration cap that was reached without the IR stabilising.
        max_iters: usize,
    },
}

impl std::fmt::Display for OptimizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptimizeError::DidNotConverge { max_iters } => write!(
                f,
                "optimization did not converge within {max_iters} iterations"
            ),
        }
    }
}

impl std::error::Error for OptimizeError {}

/// Baseline HIR cleanup: fuse consecutive `Add` / `Move` (single forward pass per block).
pub(crate) fn optimize_o0(program: HirProgram) -> HirProgram {
    HirProgram {
        insts: optimize_block_o0(program.insts),
    }
}

/// `-O1`: single pass over each block — fusion, affine / scan / clear loops, peephole on `Zero`,
/// constant propagation for dead empty-loop removal, and local `Zero`/`Add` simplification,
/// followed by a forward dead-store-elimination sweep (pass B1). The DSE sweep is idempotent
/// in isolation but can unlock further fusion/specialization when iterated by `-O2`.
pub(crate) fn optimize_o1(program: HirProgram) -> HirProgram {
    let rewritten = HirProgram {
        insts: optimize_block_o1(program.insts),
    };
    dead_store_elimination(rewritten)
}

/// `-O2`: repeat the `-O1` pipeline until the HIR reaches a fixed point (no further changes).
#[cfg(test)]
pub(crate) fn optimize_o2(program: HirProgram) -> HirProgram {
    try_optimize_o2(program).expect("optimize_o2: fixed-point optimization should converge")
}

/// `-O2` entry with explicit failure: iterates `-O1` until fixed-point or returns
/// [`OptimizeError::DidNotConverge`] when the iteration budget is exhausted.
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

/// Modular inverse of `a` mod 256. Returns `Some(x)` with `x ∈ 1..=255` and
/// `(a * x) ≡ 1 (mod 256)` when `a mod 256` is coprime to 256 (i.e. odd).
/// Returns `None` when `a mod 256` is even (no inverse exists).
pub(crate) fn invmod_256(a: i32) -> Option<i32> {
    let a = a.rem_euclid(256);
    if a & 1 == 0 {
        return None;
    }
    (1..256i32).find(|&x| (a * x).rem_euclid(256) == 1)
}

/// Affine simple loop: only `Move`/`Add`, net pointer delta 0, head cell delta
/// `d0` with `gcd(|d0| mod 256, 256) == 1` (equivalently `d0` odd) so the loop
/// terminates at an iteration count `n ≡ -v · invmod(d0, 256) (mod 256)`,
/// where `v` is the entry head value. Each body delta at offset `off` is
/// pre-scaled by `scale := (-invmod(d0, 256)) mod 256`, so the `v · f` the
/// interpreter / backend already compute equals `n · original_delta[off]`
/// without any downstream change. For `d0 ≡ -1 (mod 256)` this yields
/// `scale = 1`, i.e. the pre-generalisation behaviour.
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
    let inv = invmod_256(d0)?;
    let scale = (256 - inv).rem_euclid(256);

    let mut factors: Vec<(isize, i32)> = delta
        .into_iter()
        .filter(|(off, _)| *off != 0)
        .map(|(off, f)| (off, f.wrapping_mul(scale)))
        .filter(|(_, f)| f.rem_euclid(256) != 0)
        .collect();
    factors.sort_by_key(|(o, _)| *o);
    Some(factors)
}

fn try_scan_loop(body: &[HirInst]) -> Option<isize> {
    match body {
        [HirInst::Move(d)] if *d == 1 || *d == -1 => Some(*d),
        _ => None,
    }
}

/// Cell-clearing loop body on an 8-bit tape. The body is a single `Add(k)`
/// whose `k` is invertible mod 256 (i.e. `k` odd): iteration count
/// `n = -v · invmod(k, 256) mod 256` always terminates, and the net effect is
/// `*p = 0` regardless of `v`. The classical `[-]` is the `k = -1` case.
fn is_byte_clear_loop(body: &[HirInst]) -> bool {
    match body {
        [HirInst::Add(k)] => invmod_256(*k).is_some(),
        _ => false,
    }
}

/// When the loop head value is known at compile time and the body is an
/// affine (`try_linear_loop`) form, replace the runtime multiply/add with
/// a deterministic `Move/Add/Zero` sequence computed at compile time.
///
/// Returns `None` when:
/// - the head value is unknown (`Top` / `NonZero`) — the existing
///   `LinearMul` lowering still applies,
/// - `v == 0` — the caller should drop the loop entirely (B2 territory;
///   `try_loop_specialize` still emits a correct but redundant
///   `LinearMul` in that case),
/// - `try_linear_loop` rejects the body (not affine).
///
/// The returned sequence is emitted in "relative Move" form — the
/// pointer walks factor offsets in sorted order, emits an `Add(delta)`
/// at each, then returns to the origin before the final `Zero`. O2's
/// fixed-point loop will further fuse adjacent `Move`s if any survive.
fn try_unroll_known_head(env: &TapeState, inner: &[HirInst]) -> Option<Vec<HirInst>> {
    let v = env.value_at_ptr()?;
    if v == 0 {
        return None;
    }
    let factors = try_linear_loop(inner)?;
    let mut out: Vec<HirInst> = Vec::with_capacity(factors.len() * 2 + 2);
    let mut cur: isize = 0;
    for (off, f) in &factors {
        // `LinearMul` semantics: *(p + off) += v * f (mod 256).  With `v`
        // known, fold to a constant `Add`.  `as i8 as i32` canonicalises
        // the wrapped byte delta into `-128..=127`.
        let delta = (v as i32).wrapping_mul(*f) as i8 as i32;
        if delta == 0 {
            continue;
        }
        let step = off - cur;
        if step != 0 {
            out.push(HirInst::Move(step));
        }
        out.push(HirInst::Add(delta));
        cur = *off;
    }
    if cur != 0 {
        out.push(HirInst::Move(-cur));
    }
    out.push(HirInst::Zero);
    Some(out)
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

fn optimize_block_o1(insts: Vec<HirInst>) -> Vec<HirInst> {
    optimize_block_o1_with_parent_env(insts, false)
}

fn optimize_block_o1_with_parent_env(insts: Vec<HirInst>, nested: bool) -> Vec<HirInst> {
    let insts = fuse_add_move(insts);
    let mut env = if nested {
        TapeState::new_block()
    } else {
        TapeState::new_program()
    };
    let xfer = ConstPropXfer;

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
                    xfer.transfer_inst(&mut env, &inst);
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
                    xfer.transfer_inst(&mut env, &inst);
                    push_o1(&mut out, inst);
                }
            }

            HirInst::Loop(body) => {
                let inner = optimize_block_o1_with_parent_env(body.clone(), true);

                // B2: when the enclosing block's `TapeState` proves the
                // head cell is `Zero` at loop entry, the body never runs —
                // drop the entire `Loop` regardless of inner shape.  This
                // supersedes the old "empty body + known-zero" check and
                // also kills redundant `LinearMul` / `Scan` that
                // `try_loop_specialize` would otherwise emit for a body
                // that never executes.  The `env` fact is threaded by the
                // enclosing block's forward walk, so this is cross-block
                // information relative to the loop header.
                if env.value_at_ptr() == Some(0) {
                    i += 1;
                    continue;
                }

                if inner.is_empty() {
                    push_o1(&mut out, HirInst::Loop(inner));
                    env.clobber_all();
                    i += 1;
                    continue;
                }

                // B6: with the head value known at compile time, unroll an
                // affine loop body to a concrete `Move/Add/Zero` sequence —
                // skipping the runtime multiply the `LinearMul` lowering
                // would otherwise emit.  Tried before `try_loop_specialize`
                // so the `LinearMul` path is only reached when the head is
                // `Top`/`NonZero`.
                if let Some(unrolled) = try_unroll_known_head(&env, &inner) {
                    for inst in unrolled {
                        xfer.transfer_inst(&mut env, &inst);
                        push_o1(&mut out, inst);
                    }
                    i += 1;
                    continue;
                }

                if let Some(spec) = try_loop_specialize(&inner) {
                    xfer.transfer_inst(&mut env, &spec);
                    push_o1(&mut out, spec);
                } else {
                    push_o1(&mut out, HirInst::Loop(inner));
                    env.clobber_all();
                }
                i += 1;
            }

            other => {
                let inst = other.clone();
                xfer.transfer_inst(&mut env, &inst);
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
        // `GetByte;` prefix puts the head cell in `Top`, defeating B2's
        // known-zero loop drop so the specialisation path is exercised.
        let p = HirProgram {
            insts: vec![HirInst::GetByte, HirInst::Loop(vec![HirInst::Add(-1)])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::GetByte, HirInst::Zero]);
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
        // [->+<]  — prefixed with `GetByte` so B2 doesn't drop the
        // head-zero loop before specialisation runs.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![HirInst::GetByte, HirInst::LinearMul(vec![(1, 1)])]
        );
    }

    #[test]
    fn o1_scan_right() {
        let p = HirProgram {
            insts: vec![HirInst::GetByte, HirInst::Loop(vec![HirInst::Move(1)])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::GetByte, HirInst::Scan(1)]);
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

    // --- B3 generalisation: head delta d0 with gcd(|d0| mod 256, 256) == 1 ---

    #[test]
    fn invmod_256_covers_all_odd_values() {
        for a in (1..256).step_by(2) {
            let inv = invmod_256(a).expect("odd a must have an inverse");
            assert_eq!((a * inv).rem_euclid(256), 1, "a={a} inv={inv}");
        }
    }

    #[test]
    fn invmod_256_rejects_even_values() {
        for a in (0..256).step_by(2) {
            assert!(invmod_256(a).is_none(), "even a={a} must not invert");
        }
    }

    #[test]
    fn invmod_256_handles_negative_operands() {
        // -3 ≡ 253 (mod 256); invmod(253) must satisfy 253·x ≡ 1.
        let inv = invmod_256(-3).unwrap();
        assert_eq!((253 * inv).rem_euclid(256), 1);
    }

    #[test]
    fn o1_recognises_odd_head_delta_loop() {
        // [--->+<] : head delta -3 (odd ⇒ terminates), body writes +1 at offset 1.
        // Iteration count n = v · invmod(-(-3), 256) = v · 171 (mod 256).
        // So cell[1] += v · 171 (mod 256), i.e. factor == 171.
        // `GetByte` prefix makes the head value `Top`, so neither B6 nor B2 fires.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-3),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![HirInst::GetByte, HirInst::LinearMul(vec![(1, 171)])]
        );
    }

    #[test]
    fn o1_rejects_even_head_delta_loop() {
        // [-->+<] : head delta -2 (even ⇒ can run forever for odd v). Must stay as Loop.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-2),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert!(
            matches!(o.insts.as_slice(), [HirInst::GetByte, HirInst::Loop(_)]),
            "expected [GetByte, Loop], got {:?}",
            o.insts
        );
    }

    #[test]
    fn o1_recognises_positive_odd_head_delta_loop() {
        // [+++>+<] : head delta +3 is also odd, loop terminates.
        // invmod(3, 256) = 171 ⇒ scale = -171 mod 256 = 85.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(3),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![HirInst::GetByte, HirInst::LinearMul(vec![(1, 85)])]
        );
    }

    #[test]
    fn o1_clears_odd_decrement_loop() {
        // [---] : head decrements by 3 per iter (odd ⇒ terminates).
        // Since only the head is touched, factors == [] → specialise to Zero.
        let p = HirProgram {
            insts: vec![HirInst::GetByte, HirInst::Loop(vec![HirInst::Add(-3)])],
        };
        let o = optimize_o1(p);
        assert_eq!(o.insts, vec![HirInst::GetByte, HirInst::Zero]);
    }

    #[test]
    fn o1_rejects_even_decrement_clear_loop() {
        // [--] : head delta -2 does not clear the cell when v is odd.
        let p = HirProgram {
            insts: vec![HirInst::GetByte, HirInst::Loop(vec![HirInst::Add(-2)])],
        };
        let o = optimize_o1(p);
        assert!(
            matches!(o.insts.as_slice(), [HirInst::GetByte, HirInst::Loop(_)]),
            "expected [GetByte, Loop], got {:?}",
            o.insts
        );
    }

    #[test]
    fn o1_odd_head_delta_multi_offset() {
        // [--->+>>-<<<] : head delta -3, writes +1 at off 1 and -1 at off 3.
        // Scale = 171. factors = [(1, 171), (3, -171)]. Both ≠ 0 mod 256.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-3),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(2),
                    HirInst::Add(-1),
                    HirInst::Move(-3),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![
                HirInst::GetByte,
                HirInst::LinearMul(vec![(1, 171), (3, -171)])
            ]
        );
    }

    // --- B6: small-loop unrolling when the head value is known ---

    #[test]
    fn b6_unrolls_known_head_copy_loop() {
        // Add(5); [->+<]  — head known = 5 at loop entry, body is an affine
        // copy with factor 1 at offset 1.  B6 emits a `Move/Add/Move/Zero`
        // unroll; the leading `Add(5)` the test provides is later collapsed
        // by DSE (the `Zero` at the tail clobbers cell 0 before any read),
        // leaving exactly four instructions.
        let p = HirProgram {
            insts: vec![
                HirInst::Add(5),
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![
                HirInst::Move(1),
                HirInst::Add(5),
                HirInst::Move(-1),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn b6_unrolls_multi_offset_loop() {
        // Add(3); [->+>++<<]  — head 3, offsets 1 (+1) and 2 (+2).
        // Unrolled deltas: 3·1 = 3 at off 1, 3·2 = 6 at off 2.  Relative
        // move form walks 0→1→2→0, then clears the head.  DSE drops the
        // dead `Add(3)` on cell 0.
        let p = HirProgram {
            insts: vec![
                HirInst::Add(3),
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(1),
                    HirInst::Add(2),
                    HirInst::Move(-2),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![
                HirInst::Move(1),
                HirInst::Add(3),
                HirInst::Move(1),
                HirInst::Add(6),
                HirInst::Move(-2),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn b6_wraps_delta_to_i8_canonical_form() {
        // Add(200); [->+<].  Head lattice reports v = 200.
        // delta = (200 * 1) as i8 as i32 = -56 — canonical signed byte.
        let p = HirProgram {
            insts: vec![
                HirInst::Add(200),
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        // The critical post-B6 assertion is the `Add(-56)` in the unroll —
        // without B6 this would be a `LinearMul([(1, 1)])` that computes
        // the 200*1 product at runtime.  DSE also drops the leading
        // `Add(200)` because the tail `Zero` clobbers cell 0.
        assert_eq!(
            o.insts,
            vec![
                HirInst::Move(1),
                HirInst::Add(-56),
                HirInst::Move(-1),
                HirInst::Zero,
            ]
        );
    }

    #[test]
    fn b6_unknown_head_falls_back_to_linear_mul() {
        // No prefix Add and body reads `,` first — head value is `Top`
        // after GetByte clobbers the cell.  Must stay as `LinearMul`.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![HirInst::GetByte, HirInst::LinearMul(vec![(1, 1)])]
        );
    }

    #[test]
    fn b6_head_zero_still_drops_empty_body_loop() {
        // Regression: head == 0 at loop entry and body optimises to empty
        // (a `[-]` folds to `Zero` *inside* the loop, but an already-empty
        // body loop like `[]` stays empty and must be dropped).  B6 must
        // not interfere with the existing `inner.is_empty()` drop path.
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![])],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty());
    }

    // --- B2: cross-block zero-loop elimination driven by `TapeState` ---

    #[test]
    fn b2_drops_affine_loop_when_head_known_zero_at_entry() {
        // Fresh program → env cell[0] = Const(0). A non-empty affine body
        // that `try_loop_specialize` would otherwise lower to `LinearMul`
        // is now dropped entirely because the body never executes.
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Add(-1),
                HirInst::Move(1),
                HirInst::Add(1),
                HirInst::Move(-1),
            ])],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty(), "expected empty, got {:?}", o.insts);
    }

    #[test]
    fn b2_drops_scan_loop_when_head_known_zero() {
        // `[>]` at head == 0 is a no-op; must drop, not survive as Scan.
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Move(1)])],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty(), "expected empty, got {:?}", o.insts);
    }

    #[test]
    fn b2_drops_loop_after_arith_cancel_to_zero() {
        // Add(5); Add(-5) nets to 0 — env re-proves head == 0, so the
        // trailing loop drops.  Exercises the env-thread-through-block
        // path rather than the program-start case.
        let p = HirProgram {
            insts: vec![
                HirInst::Add(5),
                HirInst::Add(-5),
                HirInst::Loop(vec![HirInst::Add(-1)]),
            ],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty(), "expected empty, got {:?}", o.insts);
    }

    #[test]
    fn b2_drops_loop_after_explicit_zero_inst() {
        // `Zero` inst makes head == 0 an A3 fact; subsequent loop drops.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Zero,
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        // B2 drops the Loop; the `Zero` survives (current DSE does not do
        // read-reachability to end-of-program) but is cheap and sets up
        // cell 0 for any future instruction.
        assert_eq!(o.insts, vec![HirInst::GetByte, HirInst::Zero]);
    }

    #[test]
    fn b2_does_not_drop_when_head_unknown() {
        // `GetByte` leaves head at `Top` — loop must survive and
        // specialise to `LinearMul`.
        let p = HirProgram {
            insts: vec![
                HirInst::GetByte,
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(1),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let o = optimize_o1(p);
        assert_eq!(
            o.insts,
            vec![HirInst::GetByte, HirInst::LinearMul(vec![(1, 1)])]
        );
    }

    #[test]
    fn b2_drops_nested_outer_loop_when_outer_head_zero() {
        // Outer loop head known zero at program start → outer drops, and
        // with it any inner specialisation/clobber work.
        let p = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Loop(vec![HirInst::Add(-1)]),
                HirInst::Move(1),
            ])],
        };
        let o = optimize_o1(p);
        assert!(o.insts.is_empty(), "expected empty, got {:?}", o.insts);
    }
}
