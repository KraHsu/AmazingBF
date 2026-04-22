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

    /// Bounds-check a window around the current pointer and then advance
    /// `data_ptr` by `delta`.
    ///
    /// Produced by the `lir_postpone` pass (C2): a whole straight-line
    /// displacement window is verified by a single op instead of 2–3 probe
    /// `PtrAdd`s. The window `[r13 + lo_extent, r13 + hi_extent]` is asserted
    /// mapped; by the tape's contiguity every intermediate position is then
    /// also mapped, so subsequent `CellAddAt` / `CellSetAt` writes within the
    /// window need no further checks.
    ///
    /// Invariants checked by the backend in debug:
    /// - `lo_extent <= 0 <= hi_extent`
    /// - `lo_extent <= delta <= hi_extent` (so the move itself lies in the
    ///   verified window — no extra bounds check is emitted for the move)
    ///
    /// `lo_extent == 0` skips the low-side check; `hi_extent == 0` skips the
    /// high-side one. `lo_extent == hi_extent == 0` degenerates to a plain
    /// `PtrAdd(delta)` (with `delta == 0` by the second invariant).
    PtrAddChecked {
        delta: isize,
        lo_extent: isize,
        hi_extent: isize,
    },

    /// Add `n` (mod 256) to the current cell.
    CellAdd(i32),

    /// Overwrite the current cell with an immediate value.
    CellSet(u8),

    /// Add `delta` (mod 256) to the cell at `data_ptr + off`.
    ///
    /// Produced by the `lir_postpone` pass (B4 / C3). `off` is the signed
    /// displacement from the current `data_ptr`; the pass caps it at the
    /// x86 disp32 range, and codegen picks `disp8` vs. `disp32` per write.
    /// `off == 0` is canonicalised to `CellAdd(delta)` by the pass so the
    /// short `inc`/`dec` forms stay reachable.
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

    /// Same semantics as [`LirInst::Scan`], plus a static guarantee that the
    /// first `hint_bytes` cells in direction `dir` starting at `data_ptr` are
    /// already mapped inside the tape. Produced by the `lir_scan_hint` pass
    /// when a preceding [`LirInst::PtrAddChecked`] has verified a window that
    /// covers the near-term scan traversal.
    ///
    /// Codegen uses the hint to emit an unchecked fast loop body
    /// (`cmp byte [r13], 0; jz done; cmp r13, limit; jae slow; add r13, ±1;
    /// jmp top`) for up to `hint_bytes` iterations. When the boundary is
    /// reached the scan falls back to the bounds-checked `Scan` loop body,
    /// which can grow the tape if needed.
    ///
    /// `hint_bytes == 0` is allowed but degenerate and equivalent to
    /// `Scan(dir)`; the pass skips promotion in that case.
    ScanWithHint { dir: isize, hint_bytes: u32 },

    /// Clear `count` contiguous bytes starting at `[r13 + start]`.
    ///
    /// Produced by the C1 peephole when adjacent `CellSet(0)` /
    /// `CellSetAt(k, 0)` writes cover a contiguous offset range. The current
    /// codegen still lowers to `count` individual byte stores — the explicit
    /// variant exists so D2 can later swap in `rep stosb` without having to
    /// re-discover the run.
    ///
    /// Invariants:
    /// - `count >= 2` (singleton runs stay as `CellSet`/`CellSetAt`).
    /// - `[start, start + count)` fits in `i32` (inherited from the
    ///   `lir_postpone` DISP32 cap).
    ZeroRun { start: i32, count: u32 },

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
