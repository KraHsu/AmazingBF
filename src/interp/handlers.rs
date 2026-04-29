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

use crate::interp::bytecode::{
    INTERP_OP_TAG_COUNT, InterpOp, LinearMulPlan, LinearMulWithSetsPlan,
};
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

fn handle_linear_mul_with_sets<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    op: &InterpOp,
    pc: usize,
) -> Result<usize, RuntimeError> {
    if let InterpOp::LinearMulWithSets(plan) = op {
        exec_linear_mul_with_sets(interp, plan);
        Ok(pc + 1)
    } else {
        unreachable!("dispatch invariant: handle_linear_mul_with_sets only for LinearMulWithSets")
    }
}

#[inline]
fn exec_linear_mul_with_sets<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    plan: &LinearMulWithSetsPlan,
) {
    let v = interp.tape.current();
    if v == 0 {
        return;
    }
    interp.tape.set_current(0);
    for (off, f) in plan.factors.iter() {
        let delta = match *f {
            1 => v as i32,
            -1 => -(v as i32),
            f => mul_add_delta_u8(v, f as i32),
        };
        interp.tape.add_at(*off as isize, delta);
    }
    for off in plan.sets.iter() {
        interp.tape.set_at(*off as isize, 0);
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
            // F1b tiered JIT: dispatch hot loops into compiled machine
            // code. The fast path is one Vec index + tag compare per
            // back-edge, so `Failed` loops add no measurable overhead — a
            // prior HashMap-based cache cost ~25 s on mandelbrot -O3
            // (4× regression vs. plain interpret) for this exact reason.
            //
            // Profile updates are nested inside the `Cold` arm so once a
            // loop's slot is decided (Failed or Ready) we stop touching
            // the profile counters too — both decisions are sticky, so
            // the trip count never feeds another decision. On mandelbrot
            // -O3 this is the difference between 14.3 s (counters live)
            // and 8.3 s (interpreter baseline).
            #[cfg(target_os = "linux")]
            if interp.jit_enabled {
                let idx = *start_pc as usize;
                use crate::interp::engine::JitState;
                match &interp.jit_cache[idx] {
                    JitState::Failed => {
                        // Sticky reject: skip without consulting profile.
                    }
                    JitState::Ready { .. } => {
                        dispatch_jit(interp, *start_pc)?;
                        return Ok(pc + 1);
                    }
                    JitState::Cold => {
                        if let Some(ref mut profile) = interp.profile {
                            profile.record_back_edge(*start_pc);
                            if profile.trip_count(*start_pc) >= profile.threshold() {
                                compile_jit_slot(interp, *start_pc);
                                if matches!(interp.jit_cache[idx], JitState::Ready { .. }) {
                                    dispatch_jit(interp, *start_pc)?;
                                    return Ok(pc + 1);
                                }
                            }
                        }
                    }
                }
                return Ok(*start_pc as usize + 1);
            }

            // Non-JIT path: still record trip counts for profile-only
            // consumers (e.g. the `loop_profile` measurement bench).
            if let Some(ref mut profile) = interp.profile {
                profile.record_back_edge(*start_pc);
            }

            Ok(*start_pc as usize + 1)
        } else {
            Ok(pc + 1)
        }
    } else {
        unreachable!("dispatch invariant: handle_loop_end only for LoopEnd")
    }
}

/// Compile (or mark Failed) the JIT slot for the loop whose `LoopStart`
/// lives at `start_pc`. Writes exactly once per loop — `Failed` is sticky
/// so a rejected loop is never retried.
#[cfg(target_os = "linux")]
fn compile_jit_slot<I: RuntimeIo, H: HostRuntime>(interp: &mut Interpreter<I, H>, start_pc: u32) {
    use crate::interp::engine::JitState;
    use crate::interp::jit_compile::{analyse_eligibility, compile_hot_loop};

    let state = {
        let program = interp
            .program
            .as_ref()
            .expect("program is set by Interpreter::run before dispatch")
            .clone();
        let end_pc = match &program.ops[start_pc as usize] {
            InterpOp::LoopStart { end_pc } => *end_pc,
            other => unreachable!(
                "LoopEnd.start_pc must point at a LoopStart, found {:?}",
                other
            ),
        };
        let body = &program.ops[(start_pc as usize) + 1..end_pc as usize];
        match analyse_eligibility(body) {
            Some(reach) => match compile_hot_loop(body) {
                Some(buf) => JitState::Ready { buf, reach },
                None => JitState::Failed,
            },
            None => JitState::Failed,
        }
    };
    interp.jit_cache[start_pc as usize] = state;
}

/// Run the JIT-compiled body for the loop whose `LoopStart` lives at
/// `start_pc`. Pre-grows the interpreter tape to cover the body's static
/// reach, memcpys into the interpreter's persistent JIT-scratch buffer
/// (allocated/grown lazily, never mmap'd per call), calls the function,
/// then copies the modified tape back.
///
/// The eligibility contract (balanced loop, bounded reach) guarantees the
/// JIT exits with `data_ptr` at its entry position, so the same byte
/// offset can be used to restore. The persistent scratch is what
/// distinguishes this from the v1 path: each dispatch on long.b -O3 now
/// pays one memcpy in + one memcpy out + a function call, instead of
/// also paying a 4 KiB mmap + 4 KiB munmap pair.
#[cfg(target_os = "linux")]
fn dispatch_jit<I: RuntimeIo, H: HostRuntime>(
    interp: &mut Interpreter<I, H>,
    start_pc: u32,
) -> Result<(), RuntimeError> {
    use crate::interp::engine::JitState;

    let (min_off, max_off) = match &interp.jit_cache[start_pc as usize] {
        JitState::Ready { reach, .. } => *reach,
        _ => unreachable!("dispatch_jit invariant: slot must be Ready"),
    };

    // Pre-grow: ensure visited_span covers the loop's per-iteration reach,
    // so the snapshot below copies every cell the JIT will touch and the
    // JIT-side ensure_tape never fires during execution.
    interp.tape.add_at(min_off as isize, 0);
    interp.tape.add_at(max_off as isize, 0);

    // Lazy-allocate / grow the persistent scratch buffer. JitTape's mmap
    // size is page-rounded inside `JitTape::new`, so we feed it the
    // tape's required-byte count (already 4 KiB-aligned).
    let required = interp.tape.flat_required_bytes();
    let needs_alloc = match interp.jit_scratch.as_ref() {
        None => true,
        Some(t) => t.len() < required,
    };
    if needs_alloc {
        let new_tape = amazingbf_jit::JitTape::new(required.max(4096))
            .map_err(|e| RuntimeError::Jit(format!("JIT scratch mmap failed: {e}")))?;
        interp.jit_scratch = Some(new_tape);
    }

    let scratch = interp
        .jit_scratch
        .as_mut()
        .expect("jit_scratch is Some after the lazy-allocate above");
    let data_off = interp.tape.snapshot_flat_into(scratch.as_mut_slice());

    let exit = match &interp.jit_cache[start_pc as usize] {
        JitState::Ready { buf, .. } => {
            let dp = scratch.data_ptr_at(data_off);
            buf.execute_loop_fn(scratch.base(), dp, scratch.end())
        }
        _ => unreachable!("dispatch_jit invariant: slot must be Ready"),
    };

    if exit.status != 0 {
        return Err(RuntimeError::Jit(format!(
            "JIT loop returned non-zero exit code {}",
            exit.status,
        )));
    }

    // The JIT writes its final r13 into rdx (SysV 16-byte struct return),
    // landing in `exit.data_ptr`. Translate that pointer back into a byte
    // offset within the scratch buffer so `restore_from_flat` knows where
    // to set the interpreter's tape pointer. For balanced loops this equals
    // `data_off`; for unbalanced loops or scans (once eligibility is
    // relaxed) it picks up the new position.
    let final_data_off = (exit.data_ptr as usize).wrapping_sub(scratch.base() as usize);
    debug_assert!(
        final_data_off < scratch.len(),
        "JIT returned data_ptr {:?} outside scratch [{:?}, {:?})",
        exit.data_ptr,
        scratch.base(),
        scratch.end(),
    );
    interp.tape.restore_from_flat(scratch.as_slice(), final_data_off);
    Ok(())
}

/// Build the tag-indexed dispatch table. Called once per `run()`; the
/// resulting array lives on the caller's stack so the hot loop sees it as a
/// local — LLVM can keep it in registers or spill cheaply.
pub(crate) fn dispatch_table<I: RuntimeIo, H: HostRuntime>() -> [Handler<I, H>; INTERP_OP_TAG_COUNT]
{
    [
        handle_move::<I, H>,                 // 0: Move
        handle_add::<I, H>,                  // 1: Add
        handle_move_add::<I, H>,             // 2: MoveAdd
        handle_zero_move::<I, H>,            // 3: ZeroMove
        handle_put_byte::<I, H>,             // 4: PutByte
        handle_get_byte::<I, H>,             // 5: GetByte
        handle_zero::<I, H>,                 // 6: Zero
        handle_linear_mul::<I, H>,           // 7: LinearMul
        handle_linear_mul_with_sets::<I, H>, // 8: LinearMulWithSets
        handle_scan::<I, H>,                 // 9: Scan
        handle_loop_start::<I, H>,           // 10: LoopStart
        handle_loop_end::<I, H>,             // 11: LoopEnd
    ]
}
