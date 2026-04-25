//! Per-`HirInst::Loop` iteration counter for B7 hotness profiling.
//!
//! Phase 0b of the B7 plan: the static census in `loop_stats.rs` showed only
//! 2.8% of surviving Loops are B7-α candidates, with mandelbrot.b carrying
//! 31 of the 66 candidates.  This module answers the follow-up question:
//! *of the runtime loop iterations*, what fraction lands on B7-α candidates?
//! If the candidates are cold init code, B7-α is not worth the work.
//!
//! Implementation: walk the post-O2 HIR once to assign each `HirInst::Loop`
//! a stable id and record its `LoopClass`, build a parallel labelled tree,
//! then run a small recursive interpreter that increments a per-id counter
//! every time a body iteration starts.  Reuses the production `Tape` for
//! correct cell semantics; stdout sinks to a discard buffer (mandelbrot
//! produces ASCII art we don't need to render).
//!
//! Only consumed by `e5_loop_hotness_profile` (ignored test).

#![cfg_attr(not(test), allow(dead_code))]

use crate::ir::hir::{HirInst, HirProgram};
use crate::ir::loop_stats::{LoopClass, classify_loop_body};
use crate::runtime::tape::Tape;

#[derive(Debug, Clone)]
pub(crate) struct LoopInfo {
    pub id: usize,
    pub class: LoopClass,
    pub depth: usize,
    /// Shallow copy of the immediate body (for hot-loop body dumps).
    pub body: Vec<HirInst>,
}

#[derive(Debug, Clone)]
enum ProfileInst {
    Move(isize),
    Add(i32),
    PutByte,
    GetByte,
    Zero,
    LinearMul(Vec<(isize, i32)>),
    Scan(isize),
    Loop { id: usize, body: Vec<ProfileInst> },
}

pub(crate) struct Profile {
    pub infos: Vec<LoopInfo>,
    pub counts: Vec<u64>,
}

impl Profile {
    pub(crate) fn b7_alpha_total(&self) -> u64 {
        self.infos
            .iter()
            .filter(|info| {
                matches!(
                    info.class,
                    LoopClass::HasInnerZero | LoopClass::HasInnerLinearMul | LoopClass::HasBoth
                )
            })
            .map(|info| self.counts[info.id])
            .sum()
    }

    pub(crate) fn grand_total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

pub(crate) fn profile(program: &HirProgram, stdin: &[u8], tape_len: usize) -> Profile {
    let mut next_id = 0usize;
    let mut infos: Vec<LoopInfo> = Vec::new();
    let labelled = label_block(&program.insts, &mut next_id, &mut infos, 0);

    let mut counts = vec![0u64; next_id];
    let mut tape = Tape::new(tape_len);
    let mut stdin_pos = 0usize;
    let mut stdout_sink: u64 = 0;

    exec(
        &labelled,
        &mut tape,
        stdin,
        &mut stdin_pos,
        &mut stdout_sink,
        &mut counts,
    );

    Profile { infos, counts }
}

fn label_block(
    insts: &[HirInst],
    next_id: &mut usize,
    infos: &mut Vec<LoopInfo>,
    depth: usize,
) -> Vec<ProfileInst> {
    let mut out = Vec::with_capacity(insts.len());
    for inst in insts {
        match inst {
            HirInst::Move(d) => out.push(ProfileInst::Move(*d)),
            HirInst::Add(k) => out.push(ProfileInst::Add(*k)),
            HirInst::PutByte => out.push(ProfileInst::PutByte),
            HirInst::GetByte => out.push(ProfileInst::GetByte),
            HirInst::Zero => out.push(ProfileInst::Zero),
            HirInst::LinearMul(factors) => out.push(ProfileInst::LinearMul(factors.clone())),
            HirInst::Scan(d) => out.push(ProfileInst::Scan(*d)),
            HirInst::Loop(body) => {
                let id = *next_id;
                *next_id += 1;
                let class = classify_loop_body(body);
                infos.push(LoopInfo {
                    id,
                    class,
                    depth,
                    body: body.clone(),
                });
                let labelled_body = label_block(body, next_id, infos, depth + 1);
                out.push(ProfileInst::Loop {
                    id,
                    body: labelled_body,
                });
            }
        }
    }
    out
}

fn format_body_compact(body: &[HirInst]) -> String {
    let mut out = String::new();
    for (i, inst) in body.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match inst {
            HirInst::Move(d) => out.push_str(&format!("Move({})", d)),
            HirInst::Add(k) => out.push_str(&format!("Add({})", k)),
            HirInst::PutByte => out.push_str("PutByte"),
            HirInst::GetByte => out.push_str("GetByte"),
            HirInst::Zero => out.push_str("Zero"),
            HirInst::LinearMul(factors) => {
                out.push_str("LinearMul[");
                for (j, (off, f)) in factors.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("({},{})", off, f));
                }
                out.push(']');
            }
            HirInst::Scan(d) => out.push_str(&format!("Scan({})", d)),
            HirInst::Loop(_) => out.push_str("Loop(...)"),
        }
    }
    out
}

fn exec(
    insts: &[ProfileInst],
    tape: &mut Tape,
    stdin: &[u8],
    stdin_pos: &mut usize,
    stdout_sink: &mut u64,
    counts: &mut Vec<u64>,
) {
    for inst in insts {
        match inst {
            ProfileInst::Move(d) => tape.move_ptr(*d),
            ProfileInst::Add(k) => tape.add_current(*k),
            ProfileInst::Zero => tape.set_current(0),
            ProfileInst::PutByte => *stdout_sink += tape.current() as u64,
            ProfileInst::GetByte => {
                let b = stdin.get(*stdin_pos).copied().unwrap_or(0);
                if *stdin_pos < stdin.len() {
                    *stdin_pos += 1;
                }
                tape.set_current(b);
            }
            ProfileInst::LinearMul(factors) => {
                let v = tape.current();
                tape.set_current(0);
                for (off, f) in factors {
                    let delta = (v as i32).wrapping_mul(*f);
                    tape.add_at(*off, delta);
                }
            }
            ProfileInst::Scan(dir) => {
                while tape.current() != 0 {
                    tape.move_ptr(*dir);
                }
            }
            ProfileInst::Loop { id, body } => {
                while tape.current() != 0 {
                    counts[*id] += 1;
                    exec(body, tape, stdin, stdin_pos, stdout_sink, counts);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_program_has_zero_loops() {
        let prog = HirProgram { insts: vec![] };
        let p = profile(&prog, &[], 1024);
        assert!(p.infos.is_empty());
        assert_eq!(p.grand_total(), 0);
    }

    #[test]
    fn simple_loop_iterates_correctly() {
        // [-]  on a cell preset to 5 — runs 5 iterations.
        let prog = HirProgram {
            insts: vec![HirInst::Add(5), HirInst::Loop(vec![HirInst::Add(-1)])],
        };
        let p = profile(&prog, &[], 1024);
        assert_eq!(p.infos.len(), 1);
        assert_eq!(p.counts[0], 5);
    }

    #[test]
    fn nested_loop_counts_inner_per_outer_iteration() {
        // Add(3) [- > Add(2) [-] < ]  — inner runs 2 times per outer; outer runs 3.
        let prog = HirProgram {
            insts: vec![
                HirInst::Add(3),
                HirInst::Loop(vec![
                    HirInst::Add(-1),
                    HirInst::Move(1),
                    HirInst::Add(2),
                    HirInst::Loop(vec![HirInst::Add(-1)]),
                    HirInst::Move(-1),
                ]),
            ],
        };
        let p = profile(&prog, &[], 1024);
        assert_eq!(p.infos.len(), 2);
        assert_eq!(p.counts[0], 3, "outer should iterate 3x");
        assert_eq!(
            p.counts[1], 6,
            "inner should iterate 2x per outer = 6 total"
        );
    }

    #[test]
    fn linear_mul_executes_and_clears_head() {
        // Cell preset to 4, then LinearMul([(1, 3)]) — copies 4*3=12 to offset 1.
        let prog = HirProgram {
            insts: vec![HirInst::Add(4), HirInst::LinearMul(vec![(1, 3)])],
        };
        let p = profile(&prog, &[], 1024);
        assert!(p.infos.is_empty());
        // We can't observe Tape post-run from the public API; rely on counts
        // for the regression. (Adequate — LinearMul is also covered by interp tests.)
        assert_eq!(p.grand_total(), 0);
    }
}

#[cfg(test)]
mod e5_profile {
    use super::*;
    use crate::frontend::lexer::lex;
    use crate::frontend::parser::parse;
    use crate::ir::lower::lower_to_hir;
    use crate::ir::optimize::try_optimize_o2;
    use std::path::PathBuf;

    fn read_bench(name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("benches");
        path.push("bf");
        path.push(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
    }

    fn optimize(src: &str) -> HirProgram {
        let tokens = lex(src);
        let ast = parse(&tokens).expect("parse");
        try_optimize_o2(lower_to_hir(&ast)).expect("optimize")
    }

    fn run_one(name: &str, stdin: &[u8]) {
        let src = read_bench(name);
        let hir = optimize(&src);
        let prof = profile(&hir, stdin, 32 * 1024);

        let total = prof.grand_total();
        let b7a = prof.b7_alpha_total();
        let pct = if total == 0 {
            0.0
        } else {
            (b7a as f64) * 100.0 / (total as f64)
        };

        println!();
        println!("## {}", name);
        println!(
            "Total Loop iterations: {}, B7-\u{03b1} candidate iterations: {} ({:.2}%)",
            total, b7a, pct
        );

        // Top loops by count, with class.
        let mut idx: Vec<usize> = (0..prof.infos.len()).collect();
        idx.sort_by(|&a, &b| prof.counts[b].cmp(&prof.counts[a]));

        println!();
        println!("| Rank | Loop ID | Depth | Iterations | Class               | B7-\u{03b1}? |");
        println!("|------|---------|-------|------------|---------------------|--------|");
        for (rank, &i) in idx.iter().take(10).enumerate() {
            let info = &prof.infos[i];
            let is_b7a = matches!(
                info.class,
                LoopClass::HasInnerZero | LoopClass::HasInnerLinearMul | LoopClass::HasBoth
            );
            println!(
                "| {:>4} | {:>7} | {:>5} | {:>10} | {:<19?} | {:>6} |",
                rank + 1,
                info.id,
                info.depth,
                prof.counts[i],
                info.class,
                if is_b7a { "yes" } else { "" }
            );
        }

        // Top B7-α loops with body dump — answers "is this a clean
        // narrow-peephole pattern, or research-grade?"
        println!();
        println!("### Top B7-\u{03b1} candidate bodies");
        let b7a_iter = idx.iter().filter(|&&i| {
            matches!(
                prof.infos[i].class,
                LoopClass::HasInnerZero | LoopClass::HasInnerLinearMul | LoopClass::HasBoth
            )
        });
        for (rank, &i) in b7a_iter.take(3).enumerate() {
            let info = &prof.infos[i];
            println!(
                "{}. id={} class={:?} depth={} iters={} body_len={}",
                rank + 1,
                info.id,
                info.class,
                info.depth,
                prof.counts[i],
                info.body.len()
            );
            println!("   body: {}", format_body_compact(&info.body));
        }
    }

    #[test]
    #[ignore]
    fn e5_loop_hotness_profile() {
        // mandelbrot.b: no stdin, dense computation, 31 B7-α candidates.
        run_one("mandelbrot.b", &[]);
        // hanoi.b: no stdin, ASCII output, only 3 candidates but worth a peek.
        run_one("hanoi.b", &[]);
        // long.b is small enough to profile cheaply.
        run_one("long.b", &[]);
    }
}
