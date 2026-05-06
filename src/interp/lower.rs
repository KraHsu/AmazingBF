//! HIR → [`InterpProgram`] lowering.
//!
//! Flattens nested [`HirInst::Loop`] bodies into a single instruction stream
//! with absolute-PC jumps on `LoopStart` / `LoopEnd`, and performs two local
//! rewrites the dispatch loop benefits from most:
//!
//! - single-op fusion for tiny hot pairs (`Move(d); Add(k)` and
//!   `Zero; Move(d)`)
//! - straight-line operation offsets for `Move` / `Add` / `Zero` windows,
//!   lowering them to `AddAt` / `SetAt` plus one closing `Move`
//!
//! Jump targets are back-patched in a single sweep: `LoopStart` is emitted
//! with a placeholder, its index pushed onto a scratch stack, and the
//! matching `LoopEnd` writes both endpoints once it knows its own pc.
//!
//! The pass is deterministic: identical HIR always produces identical
//! `InterpProgram`, which keeps the interpreter amenable to golden tests.

use std::sync::Arc;

use crate::interp::bytecode::{
    InterpOp, InterpProgram, LinearMulPlan, LinearMulWithSetsPlan, LoopBlockPlan,
};
use crate::ir::hir::{HirInst, HirProgram};
use std::collections::BTreeMap;

/// Lower a [`HirProgram`] to a flat [`InterpProgram`] with resolved jump
/// targets and fused `MoveAdd` / `ZeroMove` superinstructions.
pub(crate) fn lower_hir_to_bytecode(program: &HirProgram) -> InterpProgram {
    let mut ops: Vec<InterpOp> = Vec::with_capacity(program.insts.len());
    let mut loop_stack: Vec<u32> = Vec::new();
    lower_block(&program.insts, &mut ops, &mut loop_stack);
    debug_assert!(
        loop_stack.is_empty(),
        "HIR loop stack must be balanced after lowering"
    );
    InterpProgram {
        ops: specialize_loop_blocks(ops),
    }
}

/// Lower a single HIR block (top-level or inside a `Loop`) by streaming ops
/// into `ops` and pushing any `LoopStart` indices onto `loop_stack` for the
/// matching `LoopEnd` to resolve.
fn lower_block(insts: &[HirInst], ops: &mut Vec<InterpOp>, loop_stack: &mut Vec<u32>) {
    let mut offset_state = OffsetLowering::default();
    for inst in insts {
        match inst {
            HirInst::Move(d) => offset_state.merge_move(*d),
            HirInst::Add(k) => offset_state.merge_add(*k),
            HirInst::Zero => offset_state.merge_zero(),
            HirInst::PutByte => {
                offset_state.flush_into(ops);
                ops.push(InterpOp::PutByte);
            }
            HirInst::GetByte => {
                offset_state.flush_into(ops);
                ops.push(InterpOp::GetByte);
            }
            HirInst::LinearMul(factors) => {
                offset_state.flush_into(ops);
                let packed: Box<[(i32, i16)]> = factors
                    .iter()
                    .map(|(off, f)| {
                        let off = clamp_i32("LinearMul offset", *off);
                        let f = clamp_i16_mod256("LinearMul factor", *f);
                        (off, f)
                    })
                    .collect();
                ops.push(InterpOp::LinearMul(Arc::new(LinearMulPlan {
                    factors: packed,
                })));
            }
            HirInst::LinearMulWithSets { factors, sets } => {
                offset_state.flush_into(ops);
                let packed_factors: Box<[(i32, i16)]> = factors
                    .iter()
                    .map(|(off, f)| {
                        let off = clamp_i32("LinearMulWithSets offset", *off);
                        let f = clamp_i16_mod256("LinearMulWithSets factor", *f);
                        (off, f)
                    })
                    .collect();
                let packed_sets: Box<[i32]> = sets
                    .iter()
                    .map(|off| clamp_i32("LinearMulWithSets set offset", *off))
                    .collect();
                ops.push(InterpOp::LinearMulWithSets(Arc::new(
                    LinearMulWithSetsPlan {
                        factors: packed_factors,
                        sets: packed_sets,
                    },
                )));
            }
            HirInst::Scan(dir) => {
                offset_state.flush_into(ops);
                let step: i8 = match dir.signum() {
                    1 => 1,
                    -1 => -1,
                    _ => panic!("Scan dir must be non-zero"),
                };
                ops.push(InterpOp::Scan(step));
            }
            HirInst::Loop(body) => {
                offset_state.flush_into(ops);
                let start_pc: u32 = ops
                    .len()
                    .try_into()
                    .expect("bytecode program length exceeds u32::MAX");
                // Placeholder; `end_pc` gets back-patched by the matching
                // `LoopEnd` below.
                ops.push(InterpOp::LoopStart { end_pc: 0 });
                loop_stack.push(start_pc);

                lower_block(body, ops, loop_stack);

                let start_pc = loop_stack
                    .pop()
                    .expect("loop stack is unbalanced: LoopEnd without matching LoopStart");
                let end_pc: u32 = ops
                    .len()
                    .try_into()
                    .expect("bytecode program length exceeds u32::MAX");
                match &mut ops[start_pc as usize] {
                    InterpOp::LoopStart { end_pc: slot } => *slot = end_pc,
                    _ => unreachable!("loop_stack index must point at LoopStart"),
                }
                ops.push(InterpOp::LoopEnd { start_pc });
            }
        }
    }
    offset_state.flush_into(ops);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingWrite {
    Add(i32),
    Set(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinearOp {
    Move(isize),
    Add(i32),
    Zero,
}

#[derive(Debug, Default)]
struct OffsetLowering {
    virt_ptr: isize,
    pending: BTreeMap<isize, PendingWrite>,
    raw: Vec<LinearOp>,
}

impl OffsetLowering {
    fn merge_move(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        self.raw.push(LinearOp::Move(delta));
        self.virt_ptr = self.virt_ptr.saturating_add(delta);
    }

    fn merge_add(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.raw.push(LinearOp::Add(delta));
        let key = self.virt_ptr;
        match self.pending.remove(&key) {
            None => {
                let norm = delta.rem_euclid(256);
                if norm != 0 {
                    self.pending.insert(key, PendingWrite::Add(norm));
                }
            }
            Some(PendingWrite::Add(prev)) => {
                let sum = prev.wrapping_add(delta).rem_euclid(256);
                if sum != 0 {
                    self.pending.insert(key, PendingWrite::Add(sum));
                }
            }
            Some(PendingWrite::Set(v)) => {
                let new_v = ((i32::from(v) + delta).rem_euclid(256)) as u8;
                self.pending.insert(key, PendingWrite::Set(new_v));
            }
        }
    }

    fn merge_zero(&mut self) {
        self.raw.push(LinearOp::Zero);
        self.pending.insert(self.virt_ptr, PendingWrite::Set(0));
    }

    fn flush_into(&mut self, ops: &mut Vec<InterpOp>) {
        if self.raw.is_empty() {
            return;
        }

        let offset_ops = self.offset_ops();
        let fallback_ops = self.fallback_ops();
        let chosen = if offset_ops.len() < fallback_ops.len() {
            offset_ops
        } else {
            fallback_ops
        };

        for op in chosen {
            push_with_tail_fusion(ops, op);
        }

        self.virt_ptr = 0;
        self.pending.clear();
        self.raw.clear();
    }

    fn offset_ops(&self) -> Vec<InterpOp> {
        let mut out = Vec::new();
        for (off, pending) in self.pending.iter() {
            let off = clamp_i32("Offset-form write offset", *off);
            match *pending {
                PendingWrite::Add(delta) => {
                    if delta == 0 {
                        continue;
                    }
                    if off == 0 {
                        out.push(InterpOp::Add(delta));
                    } else {
                        out.push(InterpOp::AddAt { off, delta });
                    }
                }
                PendingWrite::Set(val) => {
                    if off == 0 {
                        if val == 0 {
                            out.push(InterpOp::Zero);
                        } else {
                            out.push(InterpOp::Set(val));
                        }
                    } else {
                        out.push(InterpOp::SetAt { off, val });
                    }
                }
            }
        }

        if self.virt_ptr != 0 {
            out.push(InterpOp::Move(clamp_i32("Move delta", self.virt_ptr)));
        }
        out
    }

    fn fallback_ops(&self) -> Vec<InterpOp> {
        let mut out = Vec::with_capacity(self.raw.len());
        for op in self.raw.iter().copied() {
            match op {
                LinearOp::Move(delta) => {
                    out.push(InterpOp::Move(clamp_i32("Move delta", delta)));
                    fuse_recent_tail(&mut out);
                }
                LinearOp::Add(delta) => {
                    out.push(InterpOp::Add(delta));
                    fuse_recent_tail(&mut out);
                }
                LinearOp::Zero => {
                    out.push(InterpOp::Zero);
                }
            }
        }
        out
    }
}

fn push_with_tail_fusion(ops: &mut Vec<InterpOp>, op: InterpOp) {
    ops.push(op);
    fuse_recent_tail(ops);
}

fn fuse_recent_tail(ops: &mut Vec<InterpOp>) {
    if ops.len() < 2 {
        return;
    }
    let len = ops.len();
    match (ops.get(len - 2), ops.get(len - 1)) {
        (Some(InterpOp::Move(d)), Some(InterpOp::Add(k))) => {
            let (d, k) = (*d, *k);
            ops.truncate(len - 2);
            ops.push(InterpOp::MoveAdd { d, k });
        }
        (Some(InterpOp::Zero), Some(InterpOp::Move(d))) => {
            let d = *d;
            ops.truncate(len - 2);
            ops.push(InterpOp::ZeroMove(d));
        }
        _ => {}
    }
}

/// Narrow an `isize` move/offset to `i32`, panicking with a descriptive
/// context if the value overflows.
fn clamp_i32(what: &'static str, v: isize) -> i32 {
    i32::try_from(v).unwrap_or_else(|_| panic!("{what} out of i32 range: {v}"))
}

/// `try_linear_loop` already reduces every factor modulo 256, so the range
/// is `-128..=127` by the sign choice in that pass. Guard anyway in case
/// that invariant gets relaxed upstream.
fn clamp_i16_mod256(what: &'static str, v: i32) -> i16 {
    let reduced = v.rem_euclid(256);
    let signed = if reduced <= 127 {
        reduced
    } else {
        reduced - 256
    };
    i16::try_from(signed).unwrap_or_else(|_| panic!("{what} reduction out of i16: {v}"))
}

fn specialize_loop_blocks(ops: Vec<InterpOp>) -> Vec<InterpOp> {
    let mut out = ops;
    loop {
        let mut changed = false;
        let mut i = 0usize;
        while i < out.len() {
            if let InterpOp::LoopStart { end_pc } = &out[i] {
                let end = *end_pc as usize;
                if end < out.len() {
                    let body = &out[i + 1..end];
                    if is_loop_block_body(body) {
                        out[i] = InterpOp::LoopBlock(Arc::new(LoopBlockPlan {
                            after_pc: (end + 1)
                                .try_into()
                                .expect("bytecode program length exceeds u32::MAX"),
                            ops: body.to_vec().into_boxed_slice(),
                        }));
                        for slot in out.iter_mut().take(end + 1).skip(i + 1) {
                            *slot = InterpOp::NoOp;
                        }
                        changed = true;
                        i = end + 1;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if !changed {
            break;
        }
    }
    out
}

fn is_loop_block_body(body: &[InterpOp]) -> bool {
    if body.len() < 2 {
        return false;
    }
    body.iter().all(|op| {
        matches!(
            op,
            InterpOp::NoOp
                // Nested LoopBlock is not generated today, but remains safe:
                // it has no I/O and preserves the same loop-test semantics.
                | InterpOp::LoopBlock(_)
                | InterpOp::Move(_)
                | InterpOp::Add(_)
                | InterpOp::MoveAdd { .. }
                | InterpOp::ZeroMove(_)
                | InterpOp::AddAt { .. }
                | InterpOp::SetAt { .. }
                | InterpOp::Set(_)
                | InterpOp::Zero
                | InterpOp::LinearMul(_)
                | InterpOp::LinearMulWithSets(_)
                | InterpOp::Scan(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_program_lowers_to_empty_bytecode() {
        let hir = HirProgram { insts: vec![] };
        let bc = lower_hir_to_bytecode(&hir);
        assert!(bc.ops.is_empty());
    }

    #[test]
    fn simple_ops_map_one_to_one() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Add(3),
                HirInst::PutByte,
                HirInst::GetByte,
                HirInst::Zero,
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::Add(3),
                InterpOp::PutByte,
                InterpOp::GetByte,
                InterpOp::Zero,
            ]
        );
    }

    #[test]
    fn move_then_add_keeps_move_add_when_offset_form_is_not_shorter() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(2), HirInst::Add(5)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::MoveAdd { d: 2, k: 5 }]);
    }

    #[test]
    fn zero_then_move_fuses_into_zero_move() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::Move(-3)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::ZeroMove(-3)]);
    }

    #[test]
    fn chain_of_move_add_pairs_keeps_pairwise_fusion_when_tied() {
        // The fallback is two `MoveAdd`s while offset-form would need two
        // writes plus a closing move, so the cost model keeps the old shape.
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(2),
                HirInst::Move(3),
                HirInst::Add(4),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::MoveAdd { d: 1, k: 2 },
                InterpOp::MoveAdd { d: 3, k: 4 },
            ]
        );
    }

    #[test]
    fn repeated_offset_writes_win_when_they_reduce_dispatches() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(1),
                HirInst::Move(-1),
                HirInst::Add(2),
                HirInst::Move(1),
                HirInst::Add(3),
                HirInst::Move(-1),
                HirInst::Add(4),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![InterpOp::Add(6), InterpOp::AddAt { off: 1, delta: 4 },]
        );
    }

    #[test]
    fn add_without_preceding_move_stays_bare() {
        let hir = HirProgram {
            insts: vec![HirInst::Add(4)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Add(4)]);
    }

    #[test]
    fn zero_without_following_move_stays_bare() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::PutByte],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Zero, InterpOp::PutByte]);
    }

    #[test]
    fn loop_resolves_absolute_jump_targets() {
        // [>+<] (without O1): Loop(Move(1), Add(1), Move(-1))
        // Bytecode: LoopStart{end_pc=2} AddAt{off=1,delta=1} LoopEnd{start_pc=0}
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Move(1),
                HirInst::Add(1),
                HirInst::Move(-1),
            ])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::LoopStart { end_pc: 2 },
                InterpOp::AddAt { off: 1, delta: 1 },
                InterpOp::LoopEnd { start_pc: 0 },
            ]
        );
    }

    #[test]
    fn nested_loops_resolve_jump_targets_independently() {
        // [ [>] ]: Loop(Loop(Scan(1)))
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Loop(vec![HirInst::Scan(1)])])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        // outer: 0 LoopStart → 4 LoopEnd
        // inner: 1 LoopStart → 3 LoopEnd, body at pc=2
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::LoopStart { end_pc: 4 },
                InterpOp::LoopStart { end_pc: 3 },
                InterpOp::Scan(1),
                InterpOp::LoopEnd { start_pc: 1 },
                InterpOp::LoopEnd { start_pc: 0 },
            ]
        );
    }

    #[test]
    fn straight_line_unbalanced_loop_becomes_loop_block() {
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![
                HirInst::Add(-1),
                HirInst::Move(200_713),
                HirInst::Add(1),
                HirInst::Move(-200_724),
            ])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        match bc.ops.as_slice() {
            [InterpOp::LoopBlock(plan), InterpOp::NoOp, InterpOp::NoOp, InterpOp::NoOp, InterpOp::NoOp] => {
                assert_eq!(plan.after_pc, 5);
                assert_eq!(
                    &*plan.ops,
                    &[
                        InterpOp::Add(-1),
                        InterpOp::MoveAdd {
                            d: 200_713,
                            k: 1,
                        },
                        InterpOp::Move(-200_724),
                    ]
                );
            }
            other => panic!("expected straight-line LoopBlock, got {other:?}"),
        }
    }

    #[test]
    fn loop_block_does_not_cross_io() {
        let hir = HirProgram {
            insts: vec![HirInst::Loop(vec![HirInst::Add(-1), HirInst::PutByte])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::LoopStart { end_pc: 3 },
                InterpOp::Add(-1),
                InterpOp::PutByte,
                InterpOp::LoopEnd { start_pc: 0 },
            ]
        );
    }

    #[test]
    fn linear_mul_packs_factors_into_plan() {
        // 171 ≡ -85 (mod 256) and -171 ≡ 85 (mod 256); the packer
        // normalises into the signed range [-128, 127] so the runtime can
        // skip an extra rem_euclid per iteration.
        let hir = HirProgram {
            insts: vec![HirInst::LinearMul(vec![(1, 171), (3, -171)])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        match bc.ops.as_slice() {
            [InterpOp::LinearMul(plan)] => {
                assert_eq!(&*plan.factors, &[(1i32, -85i16), (3, 85)]);
            }
            other => panic!("expected single LinearMul, got {other:?}"),
        }
    }

    #[test]
    fn linear_mul_factor_in_signed_range_preserves_sign() {
        // Already-signed factors like 1 / -1 pass through unchanged.
        let hir = HirProgram {
            insts: vec![HirInst::LinearMul(vec![(2, 1), (4, -1)])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        match bc.ops.as_slice() {
            [InterpOp::LinearMul(plan)] => {
                assert_eq!(&*plan.factors, &[(2i32, 1i16), (4, -1)]);
            }
            other => panic!("expected single LinearMul, got {other:?}"),
        }
    }

    #[test]
    fn scan_converts_dir_to_signum() {
        let hir = HirProgram {
            insts: vec![HirInst::Scan(-1), HirInst::Scan(1)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Scan(-1), InterpOp::Scan(1)]);
    }

    #[test]
    fn add_zero_is_skipped_even_if_upstream_forgets_to_drop_it() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(1), HirInst::Add(0), HirInst::Move(2)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        // `Add(0)` drops; the remaining straight-line pointer motion
        // collapses to the final displacement only.
        assert_eq!(bc.ops, vec![InterpOp::Move(3)]);
    }

    #[test]
    fn zero_then_nonzero_add_becomes_constant_set() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::Add(7)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::Set(7)]);
    }

    #[test]
    fn offset_writes_do_not_cross_io_barriers() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(2),
                HirInst::PutByte,
                HirInst::Move(1),
                HirInst::Add(3),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::MoveAdd { d: 1, k: 2 },
                InterpOp::PutByte,
                InterpOp::MoveAdd { d: 1, k: 3 },
            ]
        );
    }

    #[test]
    #[ignore]
    fn inspect_case7_bytecode() {
        use crate::frontend::lexer::lex;
        use crate::frontend::parser::parse;
        use crate::interp::bytecode::InterpOp;
        use crate::interp::engine::Interpreter;
        use crate::interp::jit_compile::analyse_eligibility;
        use crate::ir::lower::lower_to_hir;
        use crate::ir::optimize::try_optimize_o2;
        use crate::runtime::host::NullHost;
        use crate::runtime::io::{IoError, RuntimeIo};
        use std::collections::BTreeMap;

        struct ScriptIo {
            input: &'static [u8],
            pos: usize,
            output_len: usize,
        }

        impl RuntimeIo for ScriptIo {
            fn put_byte(&mut self, _ptr: isize, _byte: u8) -> Result<(), IoError> {
                self.output_len += 1;
                Ok(())
            }

            fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
                if let Some(&b) = self.input.get(self.pos) {
                    self.pos += 1;
                    Ok(b)
                } else {
                    Ok(255)
                }
            }
        }

        fn op_name(op: &InterpOp) -> &'static str {
            match op {
                InterpOp::NoOp => "NoOp",
                InterpOp::Move(_) => "Move",
                InterpOp::Add(_) => "Add",
                InterpOp::MoveAdd { .. } => "MoveAdd",
                InterpOp::ZeroMove(_) => "ZeroMove",
                InterpOp::AddAt { .. } => "AddAt",
                InterpOp::SetAt { .. } => "SetAt",
                InterpOp::Set(_) => "Set",
                InterpOp::PutByte => "PutByte",
                InterpOp::GetByte => "GetByte",
                InterpOp::Zero => "Zero",
                InterpOp::LinearMul(_) => "LinearMul",
                InterpOp::LinearMulWithSets(_) => "LinearMulWithSets",
                InterpOp::Scan(_) => "Scan",
                InterpOp::LoopBlock(_) => "LoopBlock",
                InterpOp::LoopStart { .. } => "LoopStart",
                InterpOp::LoopEnd { .. } => "LoopEnd",
            }
        }

        fn op_detail(op: &InterpOp) -> String {
            match op {
                InterpOp::NoOp => "NoOp".to_string(),
                InterpOp::Move(d) => format!("Move({d})"),
                InterpOp::Add(k) => format!("Add({k})"),
                InterpOp::MoveAdd { d, k } => format!("MoveAdd(d={d},k={k})"),
                InterpOp::ZeroMove(d) => format!("ZeroMove({d})"),
                InterpOp::AddAt { off, delta } => format!("AddAt(off={off},delta={delta})"),
                InterpOp::SetAt { off, val } => format!("SetAt(off={off},val={val})"),
                InterpOp::Set(v) => format!("Set({v})"),
                InterpOp::LinearMul(plan) => format!("LinearMul(factors={})", plan.factors.len()),
                InterpOp::LinearMulWithSets(plan) => {
                    format!(
                        "LinearMulWithSets(factors={},sets={})",
                        plan.factors.len(),
                        plan.sets.len()
                    )
                }
                InterpOp::Scan(d) => format!("Scan({d})"),
                InterpOp::LoopBlock(plan) => format!("LoopBlock(ops={})", plan.ops.len()),
                InterpOp::LoopStart { end_pc } => format!("LoopStart(end={end_pc})"),
                InterpOp::LoopEnd { start_pc } => format!("LoopEnd(start={start_pc})"),
                other => op_name(other).to_string(),
            }
        }

        fn counts(ops: &[InterpOp]) -> BTreeMap<&'static str, usize> {
            let mut out = BTreeMap::new();
            for op in ops {
                *out.entry(op_name(op)).or_insert(0) += 1;
            }
            out
        }

        let src = include_str!("../../tests/cases/7.bf");
        let input = include_bytes!("../../tests/cases/7.in");
        let tokens = lex(src);
        let ast = parse(&tokens).expect("parse");
        let hir = try_optimize_o2(lower_to_hir(&ast)).expect("optimize");
        let bc = lower_hir_to_bytecode(&hir);

        eprintln!(
            "case7 static: source_bytes={} tokens={} ast_top={} hir_top={} bytecode_ops={}",
            src.len(),
            tokens.len(),
            ast.len(),
            hir.insts.len(),
            bc.ops.len()
        );
        eprintln!("bytecode opcode counts: {:?}", counts(&bc.ops));

        let mut loops = Vec::new();
        for (pc, op) in bc.ops.iter().enumerate() {
            match op {
                InterpOp::LoopStart { end_pc } => {
                    let end = *end_pc as usize;
                    loops.push((pc, end, end.saturating_sub(pc + 1)));
                }
                InterpOp::LoopBlock(plan) => {
                    let end = plan.after_pc as usize;
                    loops.push((pc, end, plan.ops.len()));
                }
                _ => {}
            }
        }
        loops.sort_by_key(|&(pc, _, _)| pc);
        eprintln!("loops={}", loops.len());
        for &(pc, end, body_len) in loops.iter().take(30) {
            let (body, sample) = match &bc.ops[pc] {
                InterpOp::LoopBlock(plan) => {
                    let sample = plan.ops.iter().take(12).map(op_detail).collect::<Vec<_>>();
                    (&plan.ops[..], sample)
                }
                _ => {
                    let body = &bc.ops[pc + 1..end];
                    let sample = body.iter().take(12).map(op_detail).collect::<Vec<_>>();
                    (body, sample)
                }
            };
            eprintln!(
                "loop pc={pc} end={end} body_len={body_len} counts={:?} sample={:?}",
                counts(body),
                sample
            );
        }

        let io = ScriptIo {
            input,
            pos: 0,
            output_len: 0,
        };
        let mut interp = Interpreter::new(32 * 1024, io, NullHost::new());
        interp.enable_profiling(1);
        interp.run(&hir).expect("run");
        let profile = interp.profile().expect("profile");
        let mut hot = loops
            .iter()
            .map(|&(pc, end, body_len)| (profile.trip_count(pc as u32), pc, end, body_len))
            .filter(|&(trips, _, _, _)| trips > 0)
            .collect::<Vec<_>>();
        hot.sort_by(|a, b| b.cmp(a));
        eprintln!(
            "runtime: input_read={} output_len={}",
            interp.io.pos, interp.io.output_len
        );
        eprintln!("hot loop entries={}", hot.len());
        for &(trips, pc, end, body_len) in hot.iter().take(30) {
            let (body, sample) = match &bc.ops[pc] {
                InterpOp::LoopBlock(plan) => {
                    let sample = plan.ops.iter().take(12).map(op_detail).collect::<Vec<_>>();
                    (&plan.ops[..], sample)
                }
                _ => {
                    let body = &bc.ops[pc + 1..end];
                    let sample = body.iter().take(12).map(op_detail).collect::<Vec<_>>();
                    (body, sample)
                }
            };
            let eligibility = analyse_eligibility(body);
            eprintln!(
                "hot trips={trips} pc={pc} end={end} body_len={body_len} eligible={eligibility:?} counts={:?} sample={:?}",
                counts(body),
                sample
            );
        }
    }
}
