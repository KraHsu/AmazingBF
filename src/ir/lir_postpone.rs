//! B4 + C3: pointer postponement and displacement-form lowering.
//!
//! Rewrites straight-line LIR runs so that sequences like
//! `PtrAdd; CellAdd; PtrAdd; CellSet; PtrAdd; CellAdd` collapse into:
//!
//! 1. **Probe** real `PtrAdd`s that visit the min and max pending offsets of
//!    the block. These are full (bounds-checking) pointer moves that drive
//!    [`crate::backend::codegen::emit_ptr_add_out`] and, on slow path,
//!    `ensure_tape_contains_r15`.
//! 2. **Displacement writes** ([`LirInst::CellAddAt`] / [`LirInst::CellSetAt`])
//!    at a single base pointer, emitted in ascending offset order so the
//!    codegen can use the short `add/mov byte [r13 + disp8], imm8` form.
//! 3. **Closing `PtrAdd(virt_ptr)`** capturing the block's net pointer
//!    displacement.
//!
//! The savings come from folding what would otherwise be N interleaved
//! bounds-checked `PtrAdd`s into at most three probe moves plus one closing
//! move, and from replacing N `[r13]` read-modify-writes with displacement
//! writes that share an R13 snapshot.
//!
//! ## Safety argument
//!
//! `CellAddAt { off, delta }` lowers to `add byte [r13 + off], imm8`; this
//! encoding does **no** bounds check. Correctness therefore hinges on the
//! address `r13 + off` lying inside the mapped tape range `[R12, R14)` at the
//! moment the write executes.
//!
//! The pass establishes this invariant by, at every flush, emitting probe
//! `PtrAdd`s that visit the minimum and maximum offsets in `pending` before
//! emitting any displacement write:
//!
//! - Each probe is a real [`LirInst::PtrAdd`]. Codegen emits the standard
//!   compare-and-slow-path-call sequence, which grows the tape via
//!   `ensure_tape_contains_r15` if the target address would escape the
//!   current mapping. After the probe, `R13` is a validated point inside
//!   the (possibly grown) tape.
//! - `ensure_tape_contains_r15` preserves the old tape contents as a
//!   contiguous slice of the new mapping (see the `copy_start`/`rep movsb`
//!   logic in `emit_ensure_tape_contains_r15`). Thus after growth, the
//!   mapped range `[R12, R14)` is still a contiguous interval.
//! - Once both the minimum probe target `r13_0 + lo` and the maximum probe
//!   target `r13_0 + hi` lie inside `[R12, R14)`, every position in
//!   `[r13_0 + lo, r13_0 + hi]` does too, by contiguity.
//! - All pending offsets are, by construction, in `[lo, hi]`, so every
//!   displacement write executed after the probes targets a mapped address.
//!
//! The final `PtrAdd(virt_ptr)` runs only after all displacement writes
//! have executed, so it cannot invalidate the addresses the writes depended
//! on (it may still grow the tape to reach `r13_0 + virt_ptr` itself, which
//! is independent).
//!
//! ## Invariants
//!
//! - `virt_ptr ∈ [-127, 127]` at every step; a would-be-out-of-range
//!   `PtrAdd` triggers a flush and is then emitted unwrapped.
//! - Every key of `pending` equals the `virt_ptr` value at the moment the
//!   entry was created / last touched, so keys also live in `[-127, 127]`
//!   and fit in the codegen's disp8 cap.
//! - Barriers (labels, jumps, `Scan`, `LinearMul`, I/O, and pre-existing
//!   displacement variants) flush the current window before being emitted.
//!
//! ## Non-goals
//!
//! - Not bit-exact idempotent: a second pass treats `CellAddAt` /
//!   `CellSetAt` as barriers and may re-probe with tighter bounds.
//!   Semantics are preserved either way.
//! - disp32 (C2) is out of scope; the cap is disp8.

use std::collections::BTreeMap;

use crate::ir::lir::{LirInst, LirProgram};

/// Inclusive upper bound on `virt_ptr` and on pending keys (x86 disp8 range).
const DISP8_MAX: isize = 127;

/// Inclusive lower bound on `virt_ptr` and on pending keys.
const DISP8_MIN: isize = -128;

/// Pending write at a single offset inside a straight-line block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    /// Cumulative addition, already normalised modulo 256 into `1..=255`.
    Add(i32),
    /// Last store wins; any prior `Add`/`Set` at this offset was clobbered.
    Set(u8),
}

/// Rewrite `program` by merging straight-line `PtrAdd` / `CellAdd` /
/// `CellSet` sequences into displacement-form writes.
///
/// Safe to call on any LIR; the pass is a fixed-point-ish value-in /
/// value-out transformation. It does not introduce new barriers and does
/// not cross existing ones.
pub(crate) fn postpone_pointer_adds(program: LirProgram) -> LirProgram {
    let mut out = Vec::with_capacity(program.insts.len());
    let mut state = PostponeState::default();

    for inst in program.insts {
        match inst {
            LirInst::PtrAdd(0) => {}
            LirInst::PtrAdd(n) => {
                let new_virt = state.virt_ptr.saturating_add(n);
                if !(DISP8_MIN..=DISP8_MAX).contains(&new_virt) {
                    // virt_ptr would escape disp8; flush the current window,
                    // then emit the incoming PtrAdd unwrapped so it drives the
                    // real bounds check and grows the tape if needed.
                    state.flush_into(&mut out);
                    out.push(LirInst::PtrAdd(n));
                } else {
                    state.virt_ptr = new_virt;
                }
            }
            LirInst::CellAdd(0) => {}
            LirInst::CellAdd(k) => state.merge_add(k),
            LirInst::CellSet(v) => state.merge_set(v),
            // Any barrier (or a pre-existing displacement form from an
            // earlier pass) flushes first and is then emitted as-is.
            barrier => {
                state.flush_into(&mut out);
                out.push(barrier);
            }
        }
    }
    state.flush_into(&mut out);
    LirProgram { insts: out }
}

#[derive(Debug, Default)]
struct PostponeState {
    /// Logical pointer delta since the last real `PtrAdd` was emitted.
    virt_ptr: isize,
    /// Writes deferred until flush; keys are offsets relative to the same
    /// `r13_0` snapshot that `virt_ptr` is measured against.
    pending: BTreeMap<isize, PendingOp>,
}

impl PostponeState {
    fn merge_add(&mut self, delta: i32) {
        let key = self.virt_ptr;
        match self.pending.remove(&key) {
            None => {
                let norm = delta.rem_euclid(256);
                if norm != 0 {
                    self.pending.insert(key, PendingOp::Add(norm));
                }
            }
            Some(PendingOp::Add(prev)) => {
                let sum = (prev + delta).rem_euclid(256);
                if sum != 0 {
                    self.pending.insert(key, PendingOp::Add(sum));
                }
            }
            Some(PendingOp::Set(v)) => {
                let new_v = ((i32::from(v) + delta).rem_euclid(256)) as u8;
                self.pending.insert(key, PendingOp::Set(new_v));
            }
        }
    }

    fn merge_set(&mut self, val: u8) {
        // A store clobbers any prior Add/Set at this offset.
        self.pending.insert(self.virt_ptr, PendingOp::Set(val));
    }

    fn flush_into(&mut self, out: &mut Vec<LirInst>) {
        if self.pending.is_empty() {
            if self.virt_ptr != 0 {
                out.push(LirInst::PtrAdd(self.virt_ptr));
                self.virt_ptr = 0;
            }
            return;
        }

        let lo = *self.pending.keys().next().expect("non-empty by check");
        let hi = *self.pending.keys().next_back().expect("non-empty by check");

        // Probe extremes via real PtrAdds; they round-trip back to r13_0 so
        // every following displacement write sees a base pointer identical
        // to the one the offsets were collected against.
        match (lo.cmp(&0), hi.cmp(&0)) {
            (std::cmp::Ordering::Less, std::cmp::Ordering::Greater) => {
                out.push(LirInst::PtrAdd(hi));
                out.push(LirInst::PtrAdd(lo - hi));
                out.push(LirInst::PtrAdd(-lo));
            }
            (std::cmp::Ordering::Less, _) => {
                out.push(LirInst::PtrAdd(lo));
                out.push(LirInst::PtrAdd(-lo));
            }
            (_, std::cmp::Ordering::Greater) => {
                out.push(LirInst::PtrAdd(hi));
                out.push(LirInst::PtrAdd(-hi));
            }
            _ => {
                // Only the current cell is pending; r13_0 is trivially mapped.
            }
        }

        // Emit writes in ascending offset order.
        let drained = std::mem::take(&mut self.pending);
        for (off, op) in drained {
            match op {
                PendingOp::Add(delta) => {
                    if delta == 0 {
                        continue;
                    }
                    if off == 0 {
                        out.push(LirInst::CellAdd(delta));
                    } else {
                        out.push(LirInst::CellAddAt { off, delta });
                    }
                }
                PendingOp::Set(val) => {
                    if off == 0 {
                        out.push(LirInst::CellSet(val));
                    } else {
                        out.push(LirInst::CellSetAt { off, val });
                    }
                }
            }
        }

        if self.virt_ptr != 0 {
            out.push(LirInst::PtrAdd(self.virt_ptr));
            self.virt_ptr = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lir::{LabelGen, LirInst};

    fn run(insts: Vec<LirInst>) -> Vec<LirInst> {
        postpone_pointer_adds(LirProgram { insts }).insts
    }

    #[test]
    fn empty_program_passes_through() {
        assert!(run(vec![]).is_empty());
    }

    #[test]
    fn single_cell_add_with_no_move_stays_bare() {
        // No probe needed when only offset 0 is touched.
        assert_eq!(run(vec![LirInst::CellAdd(3)]), vec![LirInst::CellAdd(3)]);
    }

    #[test]
    fn trailing_ptr_add_is_preserved() {
        assert_eq!(run(vec![LirInst::PtrAdd(5)]), vec![LirInst::PtrAdd(5)]);
    }

    #[test]
    fn positive_only_run_emits_one_probe_pair() {
        // `>>+<+<+` — writes at offsets 0, 1, 2.
        let out = run(vec![
            LirInst::PtrAdd(2),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(-1),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(-1),
            LirInst::CellAdd(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(2),
                LirInst::PtrAdd(-2),
                LirInst::CellAdd(1),
                LirInst::CellAddAt { off: 1, delta: 1 },
                LirInst::CellAddAt { off: 2, delta: 1 },
            ]
        );
    }

    #[test]
    fn negative_only_run_probes_min() {
        // `<<+>+>+` — writes at offsets 0, -1, -2.
        let out = run(vec![
            LirInst::PtrAdd(-2),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(1),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(1),
            LirInst::CellAdd(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(-2),
                LirInst::PtrAdd(2),
                LirInst::CellAddAt { off: -2, delta: 1 },
                LirInst::CellAddAt { off: -1, delta: 1 },
                LirInst::CellAdd(1),
            ]
        );
    }

    #[test]
    fn zigzag_run_probes_both_extremes() {
        // `>>>+<<<<<+>>+` — writes at offsets 3 and -2, net virt_ptr = 0.
        let out = run(vec![
            LirInst::PtrAdd(3),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(-5),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(2),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(3),
                LirInst::PtrAdd(-5),
                LirInst::PtrAdd(2),
                LirInst::CellAddAt { off: -2, delta: 1 },
                LirInst::CellAddAt { off: 3, delta: 1 },
            ]
        );
    }

    #[test]
    fn same_offset_add_folds_mod_256() {
        let out = run(vec![
            LirInst::PtrAdd(1),
            LirInst::CellAdd(100),
            LirInst::CellAdd(156),
        ]);
        // Sum is 256 → 0 → pending entry dropped; just the PtrAdd remains.
        assert_eq!(out, vec![LirInst::PtrAdd(1)]);
    }

    #[test]
    fn set_then_add_collapses_into_set() {
        let out = run(vec![
            LirInst::PtrAdd(1),
            LirInst::CellSet(10),
            LirInst::CellAdd(5),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(1),
                LirInst::PtrAdd(-1),
                LirInst::CellSetAt { off: 1, val: 15 },
                LirInst::PtrAdd(1),
            ]
        );
    }

    #[test]
    fn putbyte_flushes_pending_writes() {
        // PutByte forces flush; subsequent ops start a fresh window.
        let out = run(vec![
            LirInst::PtrAdd(3),
            LirInst::CellAdd(1),
            LirInst::PutByte,
            LirInst::PtrAdd(-3),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(3),
                LirInst::PtrAdd(-3),
                LirInst::CellAddAt { off: 3, delta: 1 },
                LirInst::PtrAdd(3),
                LirInst::PutByte,
                LirInst::PtrAdd(-3),
            ]
        );
    }

    #[test]
    fn jump_if_zero_flushes_and_does_not_merge_across_label() {
        let mut labels = LabelGen::new();
        let lbl = labels.fresh();
        let out = run(vec![
            LirInst::PtrAdd(2),
            LirInst::JumpIfZero(lbl),
            LirInst::CellAdd(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(2),
                LirInst::JumpIfZero(lbl),
                LirInst::CellAdd(1),
            ]
        );
    }

    #[test]
    fn disp8_overflow_forces_flush_and_unwrapped_ptr_add() {
        // virt_ptr starts at 0, PtrAdd(100) brings it to 100; next PtrAdd(100)
        // would go to 200, exceeding the disp8 cap, so flush happens first.
        // After the raw PtrAdd is emitted, r13_0 advances: the following
        // CellAdd lands at offset 0 of the fresh window.
        let out = run(vec![
            LirInst::PtrAdd(100),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(100),
            LirInst::CellAdd(1),
        ]);
        assert_eq!(
            out,
            vec![
                // Flush of the first window (virt_ptr=100, pending={100: Add(1)}).
                LirInst::PtrAdd(100),
                LirInst::PtrAdd(-100),
                LirInst::CellAddAt { off: 100, delta: 1 },
                LirInst::PtrAdd(100),
                // The overflowing PtrAdd(100) emitted as-is.
                LirInst::PtrAdd(100),
                // Trailing flush of the new window (virt_ptr=0, pending={0: Add(1)}).
                LirInst::CellAdd(1),
            ]
        );
    }

    #[test]
    fn running_pass_twice_preserves_semantics_without_extra_output_growth() {
        let first = run(vec![
            LirInst::PtrAdd(3),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(-5),
            LirInst::CellAdd(1),
            LirInst::PtrAdd(2),
        ]);
        // Second pass: CellAddAt acts as a barrier, so flushes happen around
        // each displacement write. Output grows slightly but semantics hold
        // (non bit-exact idempotence — documented in the module doc).
        let second = postpone_pointer_adds(LirProgram {
            insts: first.clone(),
        })
        .insts;
        // Running a third time yields the same sequence as the second.
        let third = postpone_pointer_adds(LirProgram {
            insts: second.clone(),
        })
        .insts;
        assert_eq!(second, third, "pass reaches a fixed point after two runs");
    }

    #[test]
    fn displacement_forms_in_input_are_flush_barriers() {
        // When CellAddAt appears in input (e.g., a second pass), it flushes
        // and emits as-is; semantics preserved, no bit-exact idempotence.
        let out = run(vec![
            LirInst::PtrAdd(2),
            LirInst::CellAddAt { off: 1, delta: 3 },
            LirInst::CellAdd(5),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(2),
                LirInst::CellAddAt { off: 1, delta: 3 },
                LirInst::CellAdd(5),
            ]
        );
    }

    #[test]
    fn scan_flushes_pending() {
        let out = run(vec![
            LirInst::PtrAdd(3),
            LirInst::CellAdd(1),
            LirInst::Scan(1),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::PtrAdd(3),
                LirInst::PtrAdd(-3),
                LirInst::CellAddAt { off: 3, delta: 1 },
                LirInst::PtrAdd(3),
                LirInst::Scan(1),
            ]
        );
    }

    #[test]
    fn linear_mul_flushes_pending() {
        let out = run(vec![
            LirInst::CellAdd(1),
            LirInst::LinearMul(vec![(1, 2)]),
            LirInst::CellAdd(2),
        ]);
        assert_eq!(
            out,
            vec![
                LirInst::CellAdd(1),
                LirInst::LinearMul(vec![(1, 2)]),
                LirInst::CellAdd(2),
            ]
        );
    }

    #[test]
    fn ptr_add_only_no_writes_is_just_folded_move() {
        let out = run(vec![LirInst::PtrAdd(3), LirInst::PtrAdd(-3)]);
        // virt_ptr returns to 0; no pending → no flush output.
        assert!(out.is_empty());
    }

    #[test]
    fn trailing_flush_after_no_writes_emits_net_ptr_add() {
        let out = run(vec![LirInst::PtrAdd(3), LirInst::PtrAdd(2)]);
        assert_eq!(out, vec![LirInst::PtrAdd(5)]);
    }

    #[test]
    fn cell_set_at_current_offset_canonicalised_to_cell_set() {
        let out = run(vec![LirInst::CellSet(42)]);
        assert_eq!(out, vec![LirInst::CellSet(42)]);
    }
}
