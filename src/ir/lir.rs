//! LIR (Low-level IR): linearised control flow with labelled jumps.
//!
//! Derived from HIR by `ir::lower::lower_to_lir`. Loops are expanded into
//! labels plus `JumpIfZero` / `JumpIfNonZero` so the backend can emit native
//! conditional branches directly. LIR is the final stage before x86_64
//! codegen.

/// Opaque label identifier assigned by [`LabelGen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LabelId(
    /// Monotonic index assigned at allocation time; unique within a single program.
    pub(crate) u32,
);

/// Monotonic allocator for fresh [`LabelId`] values.
#[derive(Debug, Default)]
pub(crate) struct LabelGen {
    next: u32,
}

impl LabelGen {
    /// Create an allocator whose first label will be `LabelId(0)`.
    pub(crate) fn new() -> Self {
        Self { next: 0 }
    }

    /// Allocate the next unused label, incrementing the internal counter.
    pub(crate) fn fresh(&mut self) -> LabelId {
        let id = LabelId(self.next);
        self.next += 1;
        id
    }
}

/// LIR instruction — linearised control flow with labelled jumps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LirInst {
    /// Advance the data pointer by `n` bytes (sign-aware).
    PtrAdd(isize),

    /// Add `n` (mod 256) to the current cell.
    CellAdd(i32),

    /// Overwrite the current cell with an immediate value.
    CellSet(u8),

    /// Add `delta` (mod 256) to the cell at `data_ptr + off`.
    ///
    /// Produced by the `lir_postpone` pass (B4 / C3). `off` is the signed
    /// displacement from the current `data_ptr`; the pass guarantees
    /// `off ∈ [-127, 127]` so codegen can always use a single-byte
    /// displacement. `off == 0` is canonicalised to `CellAdd(delta)` by the
    /// pass so the short `inc`/`dec` forms stay reachable.
    CellAddAt { off: isize, delta: i32 },

    /// Overwrite the cell at `data_ptr + off` with `val`.
    ///
    /// Produced by the `lir_postpone` pass (B4 / C3) with the same `off`
    /// range guarantee as [`LirInst::CellAddAt`]. `off == 0` is
    /// canonicalised to `CellSet(val)` by the pass.
    CellSetAt { off: isize, val: u8 },

    /// Same semantics as [`crate::ir::hir::HirInst::LinearMul`].
    LinearMul(Vec<(isize, i32)>),

    /// Same semantics as [`crate::ir::hir::HirInst::Scan`] (`dir` is ±1).
    Scan(isize),

    /// Emit one byte from the current cell to stdout (Brainfuck `.`).
    PutByte,

    /// Read one byte from stdin into the current cell (Brainfuck `,`).
    GetByte,

    /// Pseudo-instruction: bind the given label to the current position.
    Label(LabelId),

    /// Branch to `label` if the current cell is zero.
    JumpIfZero(LabelId),

    /// Branch to `label` if the current cell is non-zero.
    JumpIfNonZero(LabelId),
}

/// Complete LIR program as a flat instruction sequence ready for native codegen.
#[derive(Debug, Clone, Default)]
pub(crate) struct LirProgram {
    /// Instruction stream; `Label` pseudo-instructions mark branch targets.
    pub(crate) insts: Vec<LirInst>,
}

impl LirProgram {
    /// Return the number of LIR instructions (labels included).
    pub(crate) fn len(&self) -> usize {
        self.insts.len()
    }
}
