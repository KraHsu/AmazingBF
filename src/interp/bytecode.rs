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
//! 3. Uses offset-form writes (`AddAt` / `SetAt`) for straight-line
//!    move/write windows, mirroring the compiler's operation-offset pass
//!    without exposing unchecked memory accesses to the interpreter.
//! 4. Packs `LinearMul` factors into a reference-counted plan with a
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

/// Packed factor + set list for a `LinearMulWithSets` op.
///
/// Same layout as [`LinearMulPlan`] for the factor columns, plus a
/// `Box<[i32]>` of offsets that are unconditionally zeroed (only when
/// the head cell is non-zero).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinearMulWithSetsPlan {
    pub(crate) factors: Box<[(i32, i16)]>,
    pub(crate) sets: Box<[i32]>,
}

/// Straight-line loop body executed by one interpreter dispatch.
///
/// The plan is only produced for loop bodies that contain no I/O and no
/// nested loops. It preserves BF's unbalanced-loop semantics: after each
/// body execution the loop condition is tested at the body's final pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopBlockPlan {
    pub(crate) after_pc: u32,
    pub(crate) ops: Box<[InterpOp]>,
}

/// Superinstruction form used by the HIR interpreter.
///
/// Every variant carries all state the dispatch handler needs so the engine
/// never has to peek at neighbouring ops. `LoopStart` / `LoopEnd` store
/// absolute pc indices — set by the lowering pass during a single
/// back-patching sweep — so jumps are one assignment each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterpOp {
    /// Placeholder used after whole-loop specialization to preserve absolute pc targets.
    NoOp,
    /// Advance the tape pointer by `delta` cells (sign-aware).
    Move(i32),
    /// Add `delta` (mod 256) to the current cell.
    Add(i32),
    /// Fused `Move(d); Add(k)`: move then add in one dispatched op.
    MoveAdd { d: i32, k: i32 },
    /// Fused `Zero; Move(d)`: clear the current cell, then move.
    ZeroMove(i32),
    /// Add `delta` (mod 256) to the cell at `data_ptr + off` without moving.
    AddAt { off: i32, delta: i32 },
    /// Set the cell at `data_ptr + off` without moving.
    SetAt { off: i32, val: u8 },
    /// Set the current cell to an arbitrary byte.
    Set(u8),
    /// Emit the current cell to stdout (BF `.`).
    PutByte,
    /// Read a byte from stdin into the current cell (BF `,`).
    GetByte,
    /// Set the current cell to 0 (from `[-]`-style clear loops).
    Zero,
    /// Execute a `LinearMul` plan: scale-and-add several offsets by the
    /// current cell value, then zero the head cell.
    LinearMul(Arc<LinearMulPlan>),
    /// Like [`LinearMul`](Self::LinearMul), but also zeroes a set of offsets
    /// when the head cell is non-zero. Guarded by `v != 0`.
    LinearMulWithSets(Arc<LinearMulWithSetsPlan>),
    /// `[<]` / `[>]`: while `*p != 0`, advance the pointer by `dir` (±1).
    Scan(i8),
    /// Execute a straight-line loop body until the current cell becomes zero.
    LoopBlock(Arc<LoopBlockPlan>),
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
            InterpOp::NoOp => 0,
            InterpOp::Move(_) => 1,
            InterpOp::Add(_) => 2,
            InterpOp::MoveAdd { .. } => 3,
            InterpOp::ZeroMove(_) => 4,
            InterpOp::AddAt { .. } => 5,
            InterpOp::SetAt { .. } => 6,
            InterpOp::Set(_) => 7,
            InterpOp::PutByte => 8,
            InterpOp::GetByte => 9,
            InterpOp::Zero => 10,
            InterpOp::LinearMul(_) => 11,
            InterpOp::LinearMulWithSets(_) => 12,
            InterpOp::Scan(_) => 13,
            InterpOp::LoopBlock(_) => 14,
            InterpOp::LoopStart { .. } => 15,
            InterpOp::LoopEnd { .. } => 16,
        }
    }
}

/// Number of distinct [`InterpOp`] tags. Sizes the dispatch table.
pub(crate) const INTERP_OP_TAG_COUNT: usize = 17;

/// A program in interpreter-bytecode form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InterpProgram {
    /// Flat instruction stream; `LoopStart` / `LoopEnd` carry pre-resolved
    /// absolute jump targets into this same vector.
    pub(crate) ops: Vec<InterpOp>,
}
