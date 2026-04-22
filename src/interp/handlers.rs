//! Per-opcode handler functions and the dispatch table consumed by
//! [`crate::interp::engine::Interpreter::exec_bytecode`].
//!
//! Each handler has the same signature
//! (`fn(&mut Interpreter<I, H>, &InterpOp, usize) -> Result<usize, RuntimeError>`)
//! so they fit into a single function-pointer table, indexed by
//! [`InterpOp::tag`]. The table is built once per `run()` call in
//! [`dispatch_table`]; the main loop then pays one table load + one indirect
//! call per op instead of the jump table LLVM emits from a monolithic
//! `match` — the hope being that the CPU's indirect-branch predictor learns
//! per-opcode patterns that one hot-block predictor cannot.
//!
//! Handlers pattern-match the variant they handle and fall into
//! `unreachable!()` for every other arm. The dispatch invariant (table
//! index == tag) guarantees those arms are never executed, so the panic
//! path stays cold and the straight-line body dominates.
//!
//! Jump semantics live entirely in the handlers' return value: falls
//! through with `Ok(pc + 1)`, branches by returning the absolute target.
//! Loop entry/exit uses the `end_pc` / `start_pc` fields baked in by the
//! bytecode lowering pass.

use crate::interp::bytecode::{INTERP_OP_TAG_COUNT, InterpOp, LinearMulPlan};
use crate::interp::engine::{Interpreter, RuntimeError};
use crate::runtime::host::HostRuntime;
use crate::runtime::io::RuntimeIo;

/// Handler fn pointer type: executes one op and returns the next pc.
pub(crate) type Handler<I, H> =
    fn(&mut Interpreter<I, H>, &InterpOp, usize) -> Result<usize, RuntimeError>;

/// `(v * f)` reduced to a signed `i32` delta mod 256 (Brainfuck tape). Kept
/// in sync with the identical helper that used to live in `engine.rs`; the
/// handler-based LinearMul still needs the same reduction.
#[inline]
fn mul_add_delta_u8(v: u8, f: i32) -> i32 {
    let p = (v as i32).wrapping_mul(f);
    let m = p.rem_euclid(256);
    if m <= 127 { m } else { m - 256 }
}

fn handle_move<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::Move(d) = op {
        interp.tape.move_ptr(*d as isize);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_move only for Move")
    }
}

fn handle_add<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::Add(k) = op {
        interp.tape.add_current(*k);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_add only for Add")
    }
}

fn handle_move_add<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::MoveAdd { d, k } = op {
        interp.tape.move_ptr(*d as isize);
        interp.tape.add_current(*k);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_move_add only for MoveAdd")
    }
}

fn handle_zero_move<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::ZeroMove(d) = op {
        interp.tape.set_current(0);
        interp.tape.move_ptr(*d as isize);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_zero_move only for ZeroMove")
    }
}

fn handle_put_byte<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    _op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    let ptr = interp.tape.ptr();
    let byte = interp.tape.current();
    interp.io.put_byte(ptr, byte)?;
    Ok(pc + 1)
}

fn handle_get_byte<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    _op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    let ptr = interp.tape.ptr();
    let byte = interp.io.get_byte(ptr)?;
    interp.tape.set_current(byte);
    Ok(pc + 1)
}

fn handle_zero<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    _op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    interp.tape.set_current(0);
    Ok(pc + 1)
}

fn handle_linear_mul<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::LinearMul(plan) = op {
        exec_linear_mul(interp, plan);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_linear_mul only for LinearMul")
    }
}

#[inline]
fn exec_linear_mul<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    plan: &LinearMulPlan,
) {
    let v = interp.tape.current();
    interp.tape.set_current(0);
    for (off, f) in plan.factors.iter() {
        // E3: factor ±1 is the common fused-copy shape (`[->+<]`,
        // `[->-<]`, … after B3 rescaling).  Skipping the `wrapping_mul`
        // / `rem_euclid` avoids two ALU ops per factor.  All factors
        // update via `Tape::add_at` so we pay one bounds/grow check
        // instead of two `move_ptr` calls (which also used to
        // double-count the virtual visit in the move-unit stats).
        let delta = match *f {
            1 => v as i32,
            -1 => -(v as i32),
            f => mul_add_delta_u8(v, f as i32),
        };
        interp.tape.add_at(*off as isize, delta);
    }
}

fn handle_scan<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::Scan(dir) = op {
        let step = *dir as isize;
        while interp.tape.current() != 0 {
            interp.tape.move_ptr(step);
        }
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_scan only for Scan")
    }
}

fn handle_loop_start<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::LoopStart { end_pc } = op {
        if interp.tape.current() == 0 {
            Ok(*end_pc as usize + 1)
        } else {
            Ok(pc + 1)
        }
    } else {
        unreachable!("dispatch invariant: handle_loop_start only for LoopStart")
    }
}

fn handle_loop_end<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::LoopEnd { start_pc } = op {
        if interp.tape.current() != 0 {
            Ok(*start_pc as usize + 1)
        } else {
            Ok(pc + 1)
        }
    } else {
        unreachable!("dispatch invariant: handle_loop_end only for LoopEnd")
    }
}

/// Build the tag-indexed dispatch table. Called once per `run()`; the
/// resulting array lives on the caller's stack so the hot loop sees it as a
/// local — LLVM can keep it in registers or spill cheaply.
pub(crate) fn dispatch_table<I: RuntimeIo, H: HostRuntime>() -> [Handler<I, H>; INTERP_OP_TAG_COUNT]
{
    [
        handle_move::<I, H>,       // 0: Move
        handle_add::<I, H>,        // 1: Add
        handle_move_add::<I, H>,   // 2: MoveAdd
        handle_zero_move::<I, H>,  // 3: ZeroMove
        handle_put_byte::<I, H>,   // 4: PutByte
        handle_get_byte::<I, H>,   // 5: GetByte
        handle_zero::<I, H>,       // 6: Zero
        handle_linear_mul::<I, H>, // 7: LinearMul
        handle_scan::<I, H>,       // 8: Scan
        handle_loop_start::<I, H>, // 9: LoopStart
        handle_loop_end::<I, H>,   // 10: LoopEnd
    ]
}
