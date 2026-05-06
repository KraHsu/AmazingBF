//! HIR → [`InterpProgram`] lowering.
//!
//! Flattens nested [`HirInst::Loop`] bodies into a single instruction stream
//! with absolute-PC jumps on `LoopStart` / `LoopEnd`. Straight-line runs of
//! `Move` / `Add` / `Zero` are lowered using whichever representation is
//! shorter:
//!
//! - the legacy local superinstructions (`MoveAdd`, `ZeroMove`), or
//! - interpreter offset form (`AddAt`, `SetAt`) plus one net `Move`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::interp::bytecode::{InterpOp, InterpProgram, LinearMulPlan, LinearMulWithSetsPlan};
use crate::ir::hir::{HirInst, HirProgram};

/// Lower a [`HirProgram`] to a flat [`InterpProgram`] with resolved jump
/// targets and interpreter-specific straight-line superinstructions.
pub(crate) fn lower_hir_to_bytecode(program: &HirProgram) -> InterpProgram {
    let mut ops: Vec<InterpOp> = Vec::with_capacity(program.insts.len());
    let mut loop_stack: Vec<u32> = Vec::new();
    lower_block(&program.insts, &mut ops, &mut loop_stack);
    debug_assert!(
        loop_stack.is_empty(),
        "HIR loop stack must be balanced after lowering"
    );
    InterpProgram { ops }
}

fn lower_block(insts: &[HirInst], ops: &mut Vec<InterpOp>, loop_stack: &mut Vec<u32>) {
    let mut run = Vec::new();

    for inst in insts {
        match inst {
            HirInst::Move(_) | HirInst::Add(_) | HirInst::Zero => run.push(inst.clone()),
            HirInst::PutByte => {
                flush_run(&mut run, ops);
                ops.push(InterpOp::PutByte);
            }
            HirInst::GetByte => {
                flush_run(&mut run, ops);
                ops.push(InterpOp::GetByte);
            }
            HirInst::LinearMul(factors) => {
                flush_run(&mut run, ops);
                let packed: Vec<(i32, i16)> = factors
                    .iter()
                    .map(|(off, f)| {
                        let off = clamp_i32("LinearMul offset", *off);
                        let f = clamp_i16_mod256("LinearMul factor", *f);
                        (off, f)
                    })
                    .collect();
                ops.push(pack_linear_mul(packed));
            }
            HirInst::LinearMulWithSets { factors, sets } => {
                flush_run(&mut run, ops);
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
                flush_run(&mut run, ops);
                let step: i8 = match dir.signum() {
                    1 => 1,
                    -1 => -1,
                    _ => panic!("Scan dir must be non-zero"),
                };
                ops.push(InterpOp::Scan(step));
            }
            HirInst::Loop(body) => {
                flush_run(&mut run, ops);
                let start_pc: u32 = ops
                    .len()
                    .try_into()
                    .expect("bytecode program length exceeds u32::MAX");
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

    flush_run(&mut run, ops);
}

fn flush_run(run: &mut Vec<HirInst>, ops: &mut Vec<InterpOp>) {
    if run.is_empty() {
        return;
    }

    let legacy = lower_run_legacy(run);
    let offset = lower_run_offset(run);
    if offset.len() < legacy.len() {
        ops.extend(offset);
    } else {
        ops.extend(legacy);
    }
    run.clear();
}

fn lower_run_legacy(run: &[HirInst]) -> Vec<InterpOp> {
    let mut ops = Vec::with_capacity(run.len());
    for inst in run {
        match inst {
            HirInst::Move(d) => {
                let d = clamp_i32("Move delta", *d);
                if matches!(ops.last(), Some(InterpOp::Zero)) {
                    *ops.last_mut().unwrap() = InterpOp::ZeroMove(d);
                } else {
                    ops.push(InterpOp::Move(d));
                }
            }
            HirInst::Add(k) => {
                if *k == 0 {
                    continue;
                }
                if let Some(&InterpOp::Move(d)) = ops.last() {
                    *ops.last_mut().unwrap() = InterpOp::MoveAdd { d, k: *k };
                } else {
                    ops.push(InterpOp::Add(*k));
                }
            }
            HirInst::Zero => ops.push(InterpOp::Zero),
            _ => unreachable!("run contains only Move/Add/Zero"),
        }
    }
    ops
}

fn lower_run_offset(run: &[HirInst]) -> Vec<InterpOp> {
    let mut state = OffsetState::default();
    for inst in run {
        match inst {
            HirInst::Move(d) => state.virt_ptr += clamp_i32("Move delta", *d),
            HirInst::Add(k) => state.merge_add(*k),
            HirInst::Zero => state.merge_set(0),
            _ => unreachable!("run contains only Move/Add/Zero"),
        }
    }
    state.into_ops()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCellOp {
    Add(i32),
    Set(u8),
}

#[derive(Debug, Default)]
struct OffsetState {
    virt_ptr: i32,
    pending: BTreeMap<i32, PendingCellOp>,
}

impl OffsetState {
    fn merge_add(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let key = self.virt_ptr;
        match self.pending.remove(&key) {
            None => {
                self.pending.insert(key, PendingCellOp::Add(delta));
            }
            Some(PendingCellOp::Add(prev)) => {
                let sum = prev + delta;
                if sum != 0 {
                    self.pending.insert(key, PendingCellOp::Add(sum));
                }
            }
            Some(PendingCellOp::Set(v)) => {
                let new_v = ((i32::from(v) + delta).rem_euclid(256)) as u8;
                self.pending.insert(key, PendingCellOp::Set(new_v));
            }
        }
    }

    fn merge_set(&mut self, val: u8) {
        self.pending.insert(self.virt_ptr, PendingCellOp::Set(val));
    }

    fn into_ops(self) -> Vec<InterpOp> {
        let mut ops = Vec::with_capacity(self.pending.len() + usize::from(self.virt_ptr != 0));
        for (off, op) in self.pending {
            match op {
                PendingCellOp::Add(delta) => {
                    if delta == 0 {
                        continue;
                    }
                    if off == 0 {
                        ops.push(InterpOp::Add(delta));
                    } else {
                        ops.push(InterpOp::AddAt { off, delta });
                    }
                }
                PendingCellOp::Set(val) => {
                    if off == 0 {
                        ops.push(InterpOp::Zero);
                        if val != 0 {
                            ops.push(InterpOp::Add(val.into()));
                        }
                    } else {
                        ops.push(InterpOp::SetAt { off, val });
                    }
                }
            }
        }
        if self.virt_ptr != 0 {
            ops.push(InterpOp::Move(self.virt_ptr));
        }
        ops
    }
}

fn clamp_i32(what: &'static str, v: isize) -> i32 {
    i32::try_from(v).unwrap_or_else(|_| panic!("{what} out of i32 range: {v}"))
}

fn clamp_i16_mod256(what: &'static str, v: i32) -> i16 {
    let reduced = v.rem_euclid(256);
    let signed = if reduced <= 127 {
        reduced
    } else {
        reduced - 256
    };
    i16::try_from(signed).unwrap_or_else(|_| panic!("{what} reduction out of i16: {v}"))
}

fn pack_linear_mul(factors: Vec<(i32, i16)>) -> InterpOp {
    match factors.as_slice() {
        [(off, factor)] => InterpOp::LinearMul1 {
            off: *off,
            factor: *factor,
        },
        [(off1, factor1), (off2, factor2)] => InterpOp::LinearMul2 {
            off1: *off1,
            factor1: *factor1,
            off2: *off2,
            factor2: *factor2,
        },
        _ => InterpOp::LinearMul(Arc::new(LinearMulPlan {
            factors: factors.into_boxed_slice(),
        })),
    }
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
    fn move_then_add_keeps_legacy_move_add_when_tied() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(2), HirInst::Add(5)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::MoveAdd { d: 2, k: 5 }]);
    }

    #[test]
    fn zero_then_move_keeps_legacy_zero_move_when_tied() {
        let hir = HirProgram {
            insts: vec![HirInst::Zero, HirInst::Move(-3)],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(bc.ops, vec![InterpOp::ZeroMove(-3)]);
    }

    #[test]
    fn offset_form_wins_when_it_reduces_dispatches() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(2),
                HirInst::Move(3),
                HirInst::Add(4),
                HirInst::Move(-4),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![
                InterpOp::AddAt { off: 1, delta: 2 },
                InterpOp::AddAt { off: 4, delta: 4 },
            ]
        );
    }

    #[test]
    fn loop_is_a_barrier_and_resolves_absolute_jump_targets() {
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
    fn io_flushes_run_before_emitting() {
        let hir = HirProgram {
            insts: vec![HirInst::Move(2), HirInst::Add(1), HirInst::PutByte],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![InterpOp::MoveAdd { d: 2, k: 1 }, InterpOp::PutByte]
        );
    }

    #[test]
    fn linear_mul_remains_barrier() {
        let hir = HirProgram {
            insts: vec![
                HirInst::Move(1),
                HirInst::Add(3),
                HirInst::LinearMul(vec![(1, 2)]),
            ],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert!(matches!(bc.ops[0], InterpOp::MoveAdd { d: 1, k: 3 }));
        assert!(matches!(
            bc.ops[1],
            InterpOp::LinearMul1 { off: 1, factor: 2 }
        ));
    }

    #[test]
    fn linear_mul_two_factors_uses_specialized_opcode() {
        let hir = HirProgram {
            insts: vec![HirInst::LinearMul(vec![(1, 1), (3, -1)])],
        };
        let bc = lower_hir_to_bytecode(&hir);
        assert_eq!(
            bc.ops,
            vec![InterpOp::LinearMul2 {
                off1: 1,
                factor1: 1,
                off2: 3,
                factor2: -1,
            }]
        );
    }
}
