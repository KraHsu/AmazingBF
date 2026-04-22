//! Interpreter-side bytecode (superinstruction IR).
//!
//! The HIR interpreter used to walk `&[HirInst]` recursively, paying a
//! `match` + a host-stack descent for every loop iteration. [`InterpOp`] is
//! a flat, self-contained instruction format that:
//!
//! 1. Linearises `Loop(body)` into `LoopStart` / `LoopEnd` with pre-resolved
//!    absolute jump targets, so the engine never descends into a Rust frame
//!    per BF `[` / `]`.
//! 2. Fuses the two highest-frequency HIR pairs —
//!    `Move(d); Add(k)` → `MoveAdd { d, k }` and
//!    `Zero; Move(d)` → `ZeroMove(d)`
//!    — so the dispatch table sees them as single ops.
//! 3. Packs `LinearMul` factors into a reference-counted plan with a
//!    contiguous `Box<[(i32, i16)]>` so the engine does zero allocation per
//!    iteration of `LinearMul`-specialised affine loops.
//!
//! The lowering lives in [`crate::interp::lower`]; the dispatch loop lives
//! in [`crate::interp::engine`].

use std::sync::Arc;

/// Packed factor list for a `LinearMul` op, stored behind an `Arc` so the
/// bytecode vector carries only one pointer per occurrence even when the
/// same plan appears many times after O2's fixed-point iteration.
///
/// The `(off, factor)` tuple keeps the HIR shape but narrows `factor` from
/// `i32` to `i16`: [`crate::ir::optimize::try_linear_loop`] already reduces
/// every factor modulo 256 before emitting it, so it always fits in `i16`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinearMulPlan {
    /// Pre-baked `(offset, factor-mod-256)` pairs in ascending offset order.
    pub(crate) factors: Box<[(i32, i16)]>,
}

/// Superinstruction form used by the HIR interpreter.
///
/// Every variant carries all state the dispatch handler needs so the engine
/// never has to peek at neighbouring ops. `LoopStart` / `LoopEnd` store
/// absolute pc indices — set by the lowering pass during a single
/// back-patching sweep — so jumps are one assignment each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterpOp {
    /// Advance the tape pointer by `delta` cells (sign-aware).
    Move(i32),
    /// Add `delta` (mod 256) to the current cell.
    Add(i32),
    /// Fused `Move(d); Add(k)`: move then add in one dispatched op.
    MoveAdd { d: i32, k: i32 },
    /// Fused `Zero; Move(d)`: clear the current cell, then move.
    ZeroMove(i32),
    /// Emit the current cell to stdout (BF `.`).
    PutByte,
    /// Read a byte from stdin into the current cell (BF `,`).
    GetByte,
    /// Set the current cell to 0 (from `[-]`-style clear loops).
    Zero,
    /// Execute a `LinearMul` plan: scale-and-add several offsets by the
    /// current cell value, then zero the head cell.
    LinearMul(Arc<LinearMulPlan>),
    /// `[<]` / `[>]`: while `*p != 0`, advance the pointer by `dir` (±1).
    Scan(i8),
    /// `[`: if `*p == 0`, jump to `end_pc + 1`. Otherwise fall through.
    /// `end_pc` is the absolute index of the matching [`InterpOp::LoopEnd`].
    LoopStart { end_pc: u32 },
    /// `]`: if `*p != 0`, jump to `start_pc + 1`. Otherwise fall through.
    /// `start_pc` is the absolute index of the matching
    /// [`InterpOp::LoopStart`].
    LoopEnd { start_pc: u32 },
}

impl InterpOp {
    /// Dense opcode index used by the engine's dispatch table. Must stay in
    /// sync with the `match` arms below and with
    /// [`crate::interp::handlers::DISPATCH_LEN`] / the per-type `dispatch_table`.
    ///
    /// We compile this as a safe `match` (not a `mem::transmute` off a
    /// `#[repr(u8)]` discriminant) because the crate enforces
    /// `#![forbid(unsafe_code)]`. The compiler lowers the 11-arm match to a
    /// jump table with a single bounds-limited branch, so the extra cost
    /// over a raw discriminant read is a single movzx — well below the per-op
    /// savings from replacing a big `match` with a function-pointer dispatch.
    #[inline]
    pub(crate) fn tag(&self) -> usize {
        match self {
            InterpOp::Move(_) => 0,
            InterpOp::Add(_) => 1,
            InterpOp::MoveAdd { .. } => 2,
            InterpOp::ZeroMove(_) => 3,
            InterpOp::PutByte => 4,
            InterpOp::GetByte => 5,
            InterpOp::Zero => 6,
            InterpOp::LinearMul(_) => 7,
            InterpOp::Scan(_) => 8,
            InterpOp::LoopStart { .. } => 9,
            InterpOp::LoopEnd { .. } => 10,
        }
    }
}

/// Number of distinct [`InterpOp`] tags. Sizes the dispatch table.
pub(crate) const INTERP_OP_TAG_COUNT: usize = 11;

/// A program in interpreter-bytecode form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InterpProgram {
    /// Flat instruction stream; `LoopStart` / `LoopEnd` carry pre-resolved
    /// absolute jump targets into this same vector.
    pub(crate) ops: Vec<InterpOp>,
}
