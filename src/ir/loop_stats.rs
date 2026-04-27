//! Static census of `HirInst::Loop` instances surviving `try_optimize_o2`.
//!
//! Phase 0 of the B7 (deep balanced loop / K6) plan: before extending
//! `try_linear_loop` to handle nested `Zero` / `LinearMul` in the body, count
//! how many such loops actually survive O2 in the E5 standard suite. The
//! classifier mirrors `try_linear_loop`'s rejection priority so the
//! `HasInnerZero` / `HasInnerLinearMul` / `HasBoth` buckets form a tight upper
//! bound on what a B7-α specialiser could plausibly catch.
//!
//! Only consumed by the `e5_loop_rejection_census` ignored test today; the
//! `cfg_attr(not(test), allow(dead_code))` is the same pattern used by
//! `src/ir/analysis/dataflow.rs` for analyses without a production caller yet.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;

use crate::ir::hir::{HirInst, HirProgram};
use crate::ir::optimize::invmod_256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LoopClass {
    HasIo,
    HasScan,
    NestedLoop,
    Unbalanced,
    NonInvertibleHead,
    EmptyBody,
    HasInnerZero,
    HasInnerLinearMul,
    HasBoth,
    UnexpectedAffine,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct LoopStats {
    pub total_loops: usize,
    pub by_class: BTreeMap<LoopClass, usize>,
}

impl LoopStats {
    fn record(&mut self, class: LoopClass) {
        self.total_loops += 1;
        *self.by_class.entry(class).or_insert(0) += 1;
    }

    pub(crate) fn count(&self, class: LoopClass) -> usize {
        self.by_class.get(&class).copied().unwrap_or(0)
    }

    pub(crate) fn b7_alpha_candidates(&self) -> usize {
        self.count(LoopClass::HasInnerZero)
            + self.count(LoopClass::HasInnerLinearMul)
            + self.count(LoopClass::HasBoth)
    }
}

pub(crate) fn classify(program: &HirProgram) -> LoopStats {
    let mut stats = LoopStats::default();
    classify_block(&program.insts, &mut stats);
    stats
}

fn classify_block(insts: &[HirInst], stats: &mut LoopStats) {
    for inst in insts {
        if let HirInst::Loop(body) = inst {
            stats.record(classify_loop_body(body));
            classify_block(body, stats);
        }
    }
}

pub(crate) fn classify_loop_body(body: &[HirInst]) -> LoopClass {
    if body.is_empty() {
        return LoopClass::EmptyBody;
    }

    let mut has_io = false;
    let mut has_scan = false;
    let mut has_nested = false;
    let mut has_zero = false;
    let mut has_lmul = false;

    for inst in body {
        match inst {
            HirInst::PutByte | HirInst::GetByte => has_io = true,
            HirInst::Scan(_) => has_scan = true,
            HirInst::Loop(_) => has_nested = true,
            HirInst::Zero => has_zero = true,
            HirInst::LinearMul(_) | HirInst::LinearMulWithSets { .. } => has_lmul = true,
            HirInst::Add(_) | HirInst::Move(_) => {}
        }
    }

    if has_io {
        return LoopClass::HasIo;
    }
    if has_scan {
        return LoopClass::HasScan;
    }
    if has_nested {
        return LoopClass::NestedLoop;
    }

    let mut ptr: isize = 0;
    let mut head_delta: i32 = 0;
    let mut head_clobbered = false;

    for inst in body {
        match inst {
            HirInst::Move(d) => ptr += *d,
            HirInst::Add(k) => {
                if ptr == 0 {
                    head_delta = head_delta.wrapping_add(*k);
                }
            }
            HirInst::Zero => {
                if ptr == 0 {
                    head_clobbered = true;
                }
            }
            HirInst::LinearMul(_) | HirInst::LinearMulWithSets { .. } => {
                if ptr == 0 {
                    head_clobbered = true;
                }
            }
            HirInst::PutByte | HirInst::GetByte | HirInst::Scan(_) | HirInst::Loop(_) => {
                unreachable!("filtered above")
            }
        }
    }

    if ptr != 0 {
        return LoopClass::Unbalanced;
    }
    if head_clobbered {
        // Inner Zero / LinearMul on the head cell rewrites the iteration
        // counter mid-body — same hard-mode territory as a non-invertible
        // head delta. Lump it here so B7-α candidate count stays tight.
        return LoopClass::NonInvertibleHead;
    }
    if invmod_256(head_delta).is_none() {
        return LoopClass::NonInvertibleHead;
    }

    match (has_zero, has_lmul) {
        (true, true) => LoopClass::HasBoth,
        (true, false) => LoopClass::HasInnerZero,
        (false, true) => LoopClass::HasInnerLinearMul,
        // Balanced + invertible head + only Add/Move should have been
        // specialised by try_loop_specialize; surface as a distinct bucket
        // so we can investigate if it ever appears in real programs.
        (false, false) => LoopClass::UnexpectedAffine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_body(body: Vec<HirInst>) -> LoopClass {
        classify_loop_body(&body)
    }

    #[test]
    fn empty_body_marks_empty() {
        assert_eq!(classify_body(vec![]), LoopClass::EmptyBody);
    }

    #[test]
    fn put_byte_marks_io() {
        assert_eq!(
            classify_body(vec![HirInst::Add(-1), HirInst::PutByte]),
            LoopClass::HasIo
        );
    }

    #[test]
    fn get_byte_marks_io() {
        assert_eq!(
            classify_body(vec![HirInst::GetByte, HirInst::Add(-1)]),
            LoopClass::HasIo
        );
    }

    #[test]
    fn scan_marks_has_scan() {
        assert_eq!(classify_body(vec![HirInst::Scan(1)]), LoopClass::HasScan);
    }

    #[test]
    fn nested_loop_marks_nested() {
        assert_eq!(
            classify_body(vec![HirInst::Loop(vec![HirInst::Add(-1)])]),
            LoopClass::NestedLoop
        );
    }

    #[test]
    fn nonzero_ptr_marks_unbalanced() {
        assert_eq!(
            classify_body(vec![HirInst::Move(1), HirInst::Add(-1)]),
            LoopClass::Unbalanced
        );
    }

    #[test]
    fn even_head_delta_marks_non_invertible() {
        assert_eq!(
            classify_body(vec![HirInst::Add(-2)]),
            LoopClass::NonInvertibleHead
        );
    }

    #[test]
    fn inner_zero_off_head_is_b7_alpha_candidate() {
        assert_eq!(
            classify_body(vec![
                HirInst::Add(-1),
                HirInst::Move(1),
                HirInst::Zero,
                HirInst::Move(-1),
            ]),
            LoopClass::HasInnerZero
        );
    }

    #[test]
    fn inner_zero_on_head_marks_non_invertible() {
        assert_eq!(
            classify_body(vec![HirInst::Add(-1), HirInst::Zero]),
            LoopClass::NonInvertibleHead
        );
    }

    #[test]
    fn inner_linear_mul_off_head_is_b7_alpha_candidate() {
        assert_eq!(
            classify_body(vec![
                HirInst::Add(-1),
                HirInst::Move(1),
                HirInst::LinearMul(vec![(1, 1)]),
                HirInst::Move(-1),
            ]),
            LoopClass::HasInnerLinearMul
        );
    }

    #[test]
    fn inner_linear_mul_on_head_marks_non_invertible() {
        assert_eq!(
            classify_body(vec![HirInst::Add(-1), HirInst::LinearMul(vec![(1, 1)])]),
            LoopClass::NonInvertibleHead
        );
    }

    #[test]
    fn inner_zero_and_linear_mul_marks_both() {
        assert_eq!(
            classify_body(vec![
                HirInst::Add(-1),
                HirInst::Move(1),
                HirInst::Zero,
                HirInst::Move(1),
                HirInst::LinearMul(vec![(1, 1)]),
                HirInst::Move(-2),
            ]),
            LoopClass::HasBoth
        );
    }

    #[test]
    fn classify_walks_nested_loops_independently() {
        let prog = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Add(-1),
                HirInst::Loop(vec![HirInst::Add(-2)]),
            ])],
        };
        let stats = classify(&prog);
        assert_eq!(stats.total_loops, 2);
        assert_eq!(stats.count(LoopClass::NestedLoop), 1);
        assert_eq!(stats.count(LoopClass::NonInvertibleHead), 1);
    }
}

#[cfg(test)]
mod e5_sweep {
    use super::*;
    use crate::frontend::lexer::lex;
    use crate::frontend::parser::parse;
    use crate::ir::lower::lower_to_hir;
    use crate::ir::optimize::try_optimize_o2;
    use std::path::PathBuf;

    const PROGRAMS: &[&str] = &[
        "factor.b",
        "mandelbrot.b",
        "hanoi.b",
        "dbfi.b",
        "long.b",
        "awib-0.4.b",
    ];

    fn analyze(name: &str) -> LoopStats {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("benches");
        path.push("bf");
        path.push(name);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let tokens = lex(&src);
        let ast = parse(&tokens).expect("parse");
        let hir = try_optimize_o2(lower_to_hir(&ast)).expect("optimize");
        classify(&hir)
    }

    #[test]
    #[ignore]
    fn e5_loop_rejection_census() {
        println!();
        println!(
            "| Program       | Total | I/O | Scan | Nested | Unbal | NonInv | Empty | InnerZero | InnerLMul | Both | UnxAff | B7\u{03b1} |"
        );
        println!(
            "|---------------|-------|-----|------|--------|-------|--------|-------|-----------|-----------|------|--------|------|"
        );
        let mut total_b7a = 0usize;
        let mut total_loops = 0usize;
        for name in PROGRAMS {
            let s = analyze(name);
            let b7a = s.b7_alpha_candidates();
            total_b7a += b7a;
            total_loops += s.total_loops;
            println!(
                "| {:<13} | {:>5} | {:>3} | {:>4} | {:>6} | {:>5} | {:>6} | {:>5} | {:>9} | {:>9} | {:>4} | {:>6} | {:>4} |",
                name,
                s.total_loops,
                s.count(LoopClass::HasIo),
                s.count(LoopClass::HasScan),
                s.count(LoopClass::NestedLoop),
                s.count(LoopClass::Unbalanced),
                s.count(LoopClass::NonInvertibleHead),
                s.count(LoopClass::EmptyBody),
                s.count(LoopClass::HasInnerZero),
                s.count(LoopClass::HasInnerLinearMul),
                s.count(LoopClass::HasBoth),
                s.count(LoopClass::UnexpectedAffine),
                b7a,
            );
        }
        println!();
        println!(
            "**Totals across E5 suite — Loops: {}, B7-\u{03b1} candidates: {}**",
            total_loops, total_b7a
        );
    }
}
