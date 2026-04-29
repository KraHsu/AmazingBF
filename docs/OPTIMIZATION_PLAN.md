# AmazingBF Compilation Optimization Modernization Plan

This document is the modernization roadmap for the AmazingBF toolchain's compilation optimizations. It first surveys the existing HIR / LIR / backend / interpreter optimization state, then lists the TODOs in implementation-dependency order, to serve as the reference for subsequent implementation tasks.

> Scope: HIR analysis + passes, a new LIR peephole layer, backend/codegen, interpreter + runtime, plus a section of long-term goals.
> Principles: preserve the O0–O3 CLI semantics; keep `#![forbid(unsafe_code)]`; stay `std`-only; do not cross layer boundaries.
> Bilingual: the Chinese companion lives at `docs/OPTIMIZATION_PLAN_CN.md` and is updated alongside this file.

---

## 1. Current State

### 1.1 HIR layer (`src/ir/hir.rs`, `src/ir/optimize.rs`)

- HIR variants: `Move(isize)` / `Add(i32)` / `PutByte` / `GetByte` / `Zero` / `LinearMul(Vec<(isize,i32)>)` / `LinearMulWithSets { factors: Vec<(isize,i32)>, sets: Vec<isize> }` / `Loop(Vec<HirInst>)`
- **O0** `optimize_o0` (`optimize.rs:38-42`): fuse adjacent `Move/Add`, single pass
- **O1** `optimize_o1` (`optimize.rs:60-68`): O0 fusion + `try_scan_loop` / `is_byte_clear_loop` / `try_linear_loop` (`optimize.rs:151-197`) + A1-`TapeState`-driven constant propagation (`optimize.rs:219-307`) + `push_o1` peephole (`optimize.rs:309-329`) + **B1 DSE** (`src/ir/dse.rs`) chained at the end of the pipeline
- **O2** `try_optimize_o2` (`optimize.rs:74-87`): fixed-point iteration of O1 (DSE included), capped at 4096 iterations
- **O3**: not a standalone HIR pass; `src/driver/run.rs:138-176` performs whole-program folding (no `PutByte` → `exit(0)`; no `GetByte` → run offline then `write + exit`)
- **Analysis infrastructure**: under `src/ir/analysis/`, all of A1 `TapeState` / A2 `LoopEffect` / A3 `CellLattice` four-point lattice / A4 `run_forward` forward dataflow skeleton are in place (57 unit tests). A1 + A3 drive O1's constant propagation (A3's `add_wrapping` powers the `Add` arm of `TapeState::apply`); A3 `is_zero` further drives B2 `Loop` drops and B6 small-loop unrolling. A2 / A4 remain pending — `run_forward`'s per-loop fixed-point precision is reserved for B5 LICM; `LoopEffect` is the prerequisite for B7 K6

### 1.2 LIR layer (`src/ir/lir.rs`, `src/ir/lower.rs`, `src/ir/lir_opt.rs`, `src/ir/lir_postpone.rs`, `src/ir/lir_scan_hint.rs`)

- LIR variants: `PtrAdd` / `PtrAddChecked { delta, lo_extent, hi_extent }` / `CellAdd` / `CellSet` / `CellAddAt { off, delta }` / `CellSetAt { off, val }` / `ZeroRun { start, count }` / `LinearMul` / `LinearMulWithSets` / `Scan` / `ScanWithHint { dir, hint_bytes }` / `PutByte` / `GetByte` / `Label` / `JumpIfZero` / `JumpIfNonZero`
- `lower_to_lir` (`lower.rs`) is a pure mechanical lowering that only drops zero deltas
- At `-O1` and above, three LIR passes run in sequence: `postpone_pointer_adds` (B4 pointer postponement, producing `CellAddAt` / `CellSetAt` displacement writes) → `optimize_lir` (C1 peephole: merge adjacent `PtrAdd` / `PtrAddChecked`, fold `CellSet(0);CellAdd(k)` → `CellSet(k)`, same-offset `CellAddAt` / `CellSetAt` folding, consecutive `Zero` merged into `ZeroRun`) → `promote_scan_hints` (C4 promotes `Scan` within a bounds-check window to `ScanWithHint`)
- C2 bounds-check batching is carried by `PtrAddChecked`; the backend maintains a "verified window" state machine to elide redundant bounds checks

### 1.3 Backend (`src/backend/codegen.rs`, `src/backend/codegen_common.rs`, `src/backend/x86_64/`)

- Fixed register assignment: `R12 = tape_base` / `R13 = data_ptr` / `R14 = tape_end` / `R15 = scratch`; `Rbx = output_buf_ptr` / `Rbp = output_buf_end` (D3 buffered I/O)
- G1 shared codegen: `codegen_common.rs`'s `PlatformEmitter` trait abstracts `emit_put_byte` / `emit_get_byte` / `needs_rsp_alignment()` (the three ABI-specific hooks); `emit_lir_body()` covers all ABI-neutral LIR→AsmInst translation
- D1 instruction selection: `CellAdd(±1)` → `inc` / `dec` (4-byte short form); `add/and/cmp r, imm` auto-selects `imm8` (4 bytes) or `imm32` (7 bytes); `relax.rs` iteratively shrinks long `rel32` jumps to short `rel8`
- D2 SIMD specializations: `ScanWithHint` → `repne scasb` (D2a); `ZeroRun(count≥16)` → `rep stosb` (D2b); `LinearMul` factor ±1 columns → `add/sub [r13], bl` (D2c); `LinearMul` / `LinearMulWithSets` batch bounds-check + displacement writes (D2d)
- D3 buffered I/O: 4 KiB output buffer (`mmap` anonymous page); `PutByte` hot path ~20 bytes (1/4096 triggers a `write` syscall); `GetByte` flushes first; Linux / Windows symmetric
- D4 redundant mov elimination: `LinearMul` non-±1 columns drop the superfluous `mov eax, ebx`
- D5 branch hint prefixes: `Jz` prefixed with `0x2E` (not-taken); `Jnz` prefixed with `0x3E` (taken); loop-head `Label` preceded by `Align16` pseudo-instruction (multi-byte NOP padding)

### 1.4 Interpreter + Runtime (`src/interp/engine.rs`, `src/runtime/`)

- **E1 / E2 landed**: HIR is first lowered via `src/interp/lower.rs::lower_hir_to_bytecode` to a superinstruction stream in `src/interp/bytecode.rs::InterpOp` (with `MoveAdd` / `ZeroMove` fusion and absolute-pc `LoopStart` / `LoopEnd`); `engine.rs::exec_bytecode` then dispatches through the tag-indexed function-pointer table in `src/interp/handlers.rs`. The former recursive `exec_block` + monolithic `match` has been fully replaced
- **E3 landed**: `LinearMul` handler short-circuits factor ±1 columns, skipping `wrapping_mul` / `rem_euclid`; all factors use `Tape::add_at(off, delta)` instead of the triple `move_ptr(off); add_current; move_ptr(-off)`
- **E4 landed**: `Tape` uses `Vec<u8>` with left/right halves, geometric doubling (`new_len = max(needed, old_len * 2)`), amortized O(1) per cell access
- **D3 interpreter side landed**: `BufferedStdIo` wraps process stdio with 4 KiB `BufWriter<Stdout>` + `BufReader<Stdin>`; `Interpreter::run()` explicitly calls `io.flush()?` at the tail
- Benchmark infrastructure (E5) landed: `benches/standard_suite.rs` (Criterion, interpret/execute the matslina suite) and `benches/compile_levels.rs` (custom harness, compile+run timings for `tests/cases/*.bf`). `tests/compile_artifacts.rs` only checks artifact correctness

---

## 2. Modernization Roadmap

Six phases in dependency order: A analysis infrastructure → B HIR passes → C new LIR peephole layer → D backend/codegen → E interpreter + runtime (parallelizable) → F long-term goals (not in the near-term dependency graph).

### Phase A — Analysis infrastructure (prerequisite for most other passes)

- **A1 Symbolic tape state** · **[landed]**: `src/ir/analysis/tape_state.rs`'s `TapeState` anchors at the block-entry `data_ptr`, using a `BTreeMap<isize, CellLattice>` to record the lattice value at every visited offset plus the current ptr offset. Includes `merge_in_place` (degrades to pessimistic when ptrs disagree) and `clobber_all` (triggered by `Loop` / `Scan`). `LinearMul` / `LinearMulWithSets` have precise transfer functions — head cell is zeroed, factor targets are computed via `add_wrapping(v*f)` when the head value is known (else degraded to `Top`), remaining cell facts are preserved; `LinearMulWithSets` with `v==0` is recognised as a no-op. Wired into O1's `optimize_block_o1_with_parent_env` with `ConstPropXfer: Transfer<TapeState>` driving the symbolic execution.
- **A2 Pointer-delta abstract interpretation** · **[landed]**: `src/ir/analysis/loop_effect.rs`'s `LoopEffect::analyze` produces `{ net_ptr_delta, touched, reads_cell, writes_cell, has_io }`, with `pointer_delta_range` computing `(min_off, max_off, net_delta)`. Currently a skeleton; `try_linear_loop` has not yet been migrated to it (B3 will do that).
- **A3 Cell-value abstract lattice** · **[landed]**: `src/ir/analysis/lattice.rs`'s `CellLattice { Top, NonZero, Zero, Const(u8) }` provides `meet` / `add_wrapping` / `is_zero` / `is_nonzero` / `known_u8`. The `Add(k)` arm of `TapeState::apply` now goes through `current.add_wrapping(k)`, lifting the transfer from "literal-equivalence" to proper lattice semantics; `NonZero` is preserved in cross-block facts produced by `merge_in_place`.
- **A4 Cross-block dataflow framework** · **[landed]**: `src/ir/analysis/dataflow.rs` provides the `Fact` / `Transfer` traits and the `run_forward` driver; `transfer_loop` iterates the lattice to a fixed point with a 64-iter cap and fail-safes back to `Fact::bottom()`. Implementations for both `Option<TapeState>` and `TapeState` facts are in place. Currently `run_forward` is exercised only in unit tests; the first real consumer (live-cell analysis, paving the way for B5 LICM) awaits a later phase.

**Dependency: A1 → A2 → A3 → A4 are all ready; Phase B can unfold.**

### Phase B — HIR pass expansion (depends on Phase A)

- **B1 Dead store elimination** · **[landed]**: `src/ir/dse.rs`'s `dead_store_elimination` is a forward syntactic rewrite — virtual pointer + `BTreeMap<isize, usize>` pending-write set. A write at some offset is dropped when a later write at the same offset covers it with no intervening read. `Loop` / `Scan` / `LinearMul` act as barriers that clear pending and recurse into `Loop` body; `PutByte` / `Add` commit (read-back) prior writes; `GetByte` / `Zero` unconditionally overwrite prior writes; `GetByte` itself is never droppable (input side effect). Covers the two cases `push_o1` misses: Move intervals and GetByte overwrites. Chained at the end of `optimize_o1`, automatically amplified by the O2 fixed-point loop. Final implementation is purely syntactic and does not consume A4 `run_forward` (A3 `CellLattice` is reserved for later B2 / B5 cross-block variants).
  - Files: `src/ir/dse.rs`; integration point `src/ir/optimize.rs:optimize_o1`
- **B2 Cross-block extension of known-zero-cell loop elimination** · **[landed]**: `src/ir/optimize.rs` now consults `env.value_at_ptr() == Some(0)` at the very top of the `Loop` arm, before both the `inner.is_empty()` fast path and `try_loop_specialize`. Because the parent block threads its `TapeState` forward across every instruction, "head cell provably zero at loop entry" is available as a cross-instruction fact — not only for the literal program-start case where `TapeState::new_program()` seeds `cell[0] = Const(0)`, but also for arithmetic-cancel patterns (`Add(5); Add(-5); [...]`), explicit `Zero; [...]`, and nested outer loops whose outer head is zero. Previously these bodies still lowered to a (semantically no-op but emitted) `LinearMul` / `Scan` / preserved `Loop`; B2 drops them. The pass does not yet use `run_forward` — the existing in-order env walk already delivers straight-line forward facts, and the fix-point precision `run_forward` buys (re-converging facts across loops) is reserved as the motivating consumer for B5 LICM.
  - Files: `src/ir/optimize.rs` (6 new B2 unit tests; 9 pre-existing Loop-specialisation tests were rewritten to prefix `GetByte` so they still exercise the `LinearMul` / `Scan` / `Zero` specialisation path now that B2 intercepts the known-zero case)
- **B3 LinearMul generalization (head-cell gcd relaxation)** · **[landed]**: `src/ir/optimize.rs`'s `try_linear_loop` now accepts any odd head delta `d0`. `invmod_256(d0)` (extended Euclid) computes the multiplicative inverse for the iteration count at compile time; every body factor is uniformly rescaled to `factor * invmod(-d0, 256) mod 256`, reusing the existing `LinearMul` data shape (no new variants). Even head deltas are still rejected (`gcd(|d0| mod 256, 256) ≠ 1` means the loop is either non-terminating or has non-integer iteration count). `is_byte_clear_loop` was relaxed symmetrically to recognise any odd-step `[-]` / `[--]`-equivalent form. Nested `LinearMul` bodies and the ±1-only fused-copy form remain out of scope and are deferred to B6 / B7.
  - Files: `src/ir/optimize.rs` (9 unit tests: full-table `invmod_256` coverage, negative-odd head deltas, multi-offset bodies, even rejections)
- **B4 Pointer postponement (operation offsets / offset-form)** · **[landed]**: standard industry naming ([Nayuki](https://www.nayuki.io/page/optimizing-brainfuck-compiler), [matslina](https://github.com/matslina/bfoptimization) call it "operation offsets", bfc calls it "postponing movements"). Ultimately realized as a LIR-only pass (no new HIR variants, avoiding pollution of the HIR interpreter and existing pattern detectors): `src/ir/lir_postpone.rs`'s `postpone_pointer_adds` accumulates `virt_ptr: isize` plus `pending: BTreeMap<isize, PendingOp>` over a straight-line window. On encountering a barrier (`Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte`, or a `CellAddAt` / `CellSetAt` left over from a prior pass), or when `virt_ptr` would cross the disp8 boundary, a flush is triggered; the flush first emits probe `PtrAdd`s visiting the `(lo, hi)` extremes, delegating to the backend's `ensure_tape_contains_r15` for tape doubling and relying on the contiguity of the tape mapping to guarantee that every offset in the window is already bounds-checked. disp is capped at `[-127, 127]` (disp32 deferred until C2 lands). Runs before `optimize_lir(lower_to_lir(hir))` at `-O1` and above.
  - Files: `src/ir/lir_postpone.rs` (18 unit tests including the safety proof); integration point `src/driver/run.rs:build_optimized_lir`
- **B5 Loop-invariant code motion / selective clobber** · **[landed]**: `TapeState::apply` no longer unconditionally calls `clobber_all()` on `Loop`. Instead it uses A2 `LoopEffect::analyze` for selective clobber — balanced loops (`net_ptr_delta == Some(0)`) only clear cell facts within the `touched` range, preserving facts for cells outside; unbalanced loops or unbounded `touched` ranges fall back to `clobber_all()`. All paths set `cell[ptr] = Zero` after loop exit (guaranteed by the loop condition). This is the first production consumer of A2 `LoopEffect`, removing the `#[cfg_attr(not(test), allow(dead_code))]` from `loop_effect.rs`.
  - Files: `src/ir/analysis/tape_state.rs`, `src/ir/analysis/loop_effect.rs` (3 new tests: balanced loop preserves remote cells, unbalanced loop clobbers all, empty loop preserves all facts)
- **B6 Small loop unrolling** · **[landed]**: `src/ir/optimize.rs::try_unroll_known_head` runs in the `Loop` arm before `try_loop_specialize`: when `env.value_at_ptr()` reports a known `CellLattice::Const(v)` with `v != 0` and `try_linear_loop` accepts the body, the loop is unrolled at compile time into a relative-move form — for each `(off, f)` factor emit `Move(step); Add((v * f) as i8 as i32)` (zero deltas skipped; pointer walked relatively), then `Move(-cur); Zero` at the tail. This replaces the `LinearMul` runtime `*p * factor` multiply with a pre-computed `Add`. When `v == 0` B6 returns `None` and `try_loop_specialize` still emits a (redundant) `LinearMul` — dropping that is B2 territory (Commit 3). Head values reported as `Top` / `NonZero` keep the `LinearMul` path (no regression). The `Scan` / `is_byte_clear_loop` branches of `try_loop_specialize` are unaffected because `try_linear_loop` rejects or returns empty factors for those body shapes, so B6 naturally falls through.
  - Files: `src/ir/optimize.rs` (6 unit tests: single offset, multi-offset, i8 canonicalisation, unknown head, empty body regression, `v == 0` pin of pre-B2 behaviour)
- **B7 Deep balanced loop (graduated K6 plan)** · **[B7-α (P1+P2+P3) landed]**: the original Oizys K6 relies on SymPy + Z/256 matrix eigendecomposition, which doesn't fit a std-only / `forbid(unsafe_code)` / zero-runtime-deps Rust toolchain. Phase 0 quantifies "what could K6 actually recover" via two measurement modules: `src/ir/loop_stats.rs` does a **static census** of post-O2 `HirInst::Loop`s by rejection reason; `src/ir/loop_profile.rs` is a small recursive HIR interpreter doing **per-loop runtime counting** plus body dumps for the hottest B7-α candidates. E5 suite results (`cargo test --release loop_stats::e5_sweep loop_profile::e5_profile -- --ignored --nocapture`):
  - Static: of 2361 surviving Loops only 66 (2.8%) have a B7-α-tractable shape; distribution is extremely uneven — mandelbrot.b and awib-0.4.b have 31 each, the other four programs total 4.
  - Dynamic: hanoi.b's single hot loop (class=`HasInnerZero`, body `Move(1) Zero Move(-1) Add(-1)`) accounts for **95.30%** of the program's loop iterations; long.b's single hot loop (class=`HasBoth`, body contains a dead inner LinearMul subsequently overwritten by a `Zero`) accounts for **94.17%**; mandelbrot.b's 31 static candidates only account for **1.95%** at runtime — its hot loops are all `Unbalanced` and B7 cannot help.
  - **Conclusion**: even a full K6 (Z/256 matrix eigendecomposition) can recover at most 1.95% on mandelbrot — ROI is too low. hanoi / long's hot patterns sit in a simplified subset that **does not require cross-iteration recurrence**. Split B7 into α/β: α handles those two hot loops; β is reserved for mandelbrot's cross-iteration recurrence (deprioritised).
  - **B7-β investigation conclusion** · **[investigated, deferred]**: remaining B7-α candidates (mandelbrot 1.95%, hanoi 0.11%) fail `try_linear_loop_advanced` because inner `LinearMul` heads have unknown state (`Unwritten`), causing factor targets to be marked `Tainted`. Fixing this requires cross-iteration recurrence analysis (full K6 algorithm), disproportionate complexity for <2% gain. The `NestedLoop` class (858 surviving loops) contains inner loops that are genuinely non-linearizable (I/O, scans, unbalanced, etc.), beyond B7-β's reach.
  - **B7-α design (three phases, all landed)**:
    - **B7-α-P1** · **[landed]**: added HIR variant `LinearMulWithSets { factors: Vec<(isize,i32)>, sets: Vec<isize> }` with semantics `v=*p; *p=0; for (off,f) in factors: cell[ptr+off] += v*f; if v != 0 { for off in sets: cell[ptr+off] = 0; }`; `try_linear_loop_with_sets` accepts `Zero` in the body (at ptr ≠ 0), keeps the head-cell invertibility requirement. Catches hanoi #559 (95% of hanoi iterations). LIR has a matching variant; backend emits `cmp/jz` guard to skip the sets section; interpreter handler supports it.
    - **B7-α-P2** · **[landed]**: `try_linear_loop_advanced` upgrades to a per-cell four-state machine (`Unwritten` / `LinearAdd(coef)` / `Set(val)` / `Tainted`); accepts inner `LinearMul` / `LinearMulWithSets` when the head cell is provably `Set(c)` (deterministic expansion) or `Set(0)` (no-op skip); subsequent `Zero` clears `Tainted` marks. When no `Tainted` survives, extracts factors + sets reusing P1's `LinearMulWithSets` variant — no new HIR/LIR/backend code needed. Catches long.b's `HasBoth`-class hot loop (94% of iterations).
    - **B7-α-P3** · **[landed]**: shipped together with P1. `LinearMulWithSets` is a standalone LIR variant; the backend emits `cmp byte [r13], 0; jz skip_sets` after the factors section (~5 bytes overhead). Both backends (Linux / Windows) implement this symmetrically.
  - Files: `src/ir/optimize.rs` (`try_linear_loop_with_sets` + `try_linear_loop_advanced` + 18 B7-α unit tests), `src/ir/hir.rs`, `src/ir/lir.rs`, `src/ir/lower.rs`, `src/interp/bytecode.rs`, `src/interp/handlers.rs`, `src/interp/lower.rs`, `src/backend/codegen.rs`, `src/backend/x86_64/windows.rs`; measurement modules `src/ir/loop_stats.rs` (13 unit tests), `src/ir/loop_profile.rs` (4 unit tests).

**Dependencies: B1 / B2 / B3 / B4 / B5 / B6 / B7-α have landed; B7-β investigated and deferred (ROI <2%).**

### Phase C — New LIR peephole layer

Goal: add `src/ir/lir_opt.rs` after `src/ir/lower.rs` to provide a no-analysis peephole pass.

- **C1 Base LIR peephole pass** · **[landed]**: merge adjacent `PtrAdd`, eliminate zero deltas, fold `CellSet(0); CellAdd(k)` → `CellSet(k)` and `CellSet(a); CellSet(b)` → `CellSet(b)`. Landed in `src/ir/lir_opt.rs`, chained after `lower_to_lir`; `Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte` serve as natural barriers.
- **C2 Bounds-check hoisting / batching** · **[landed]**: LIR gained `LirInst::PtrAddChecked { delta, lo_extent, hi_extent }` (`src/ir/lir.rs`) carrying the semantics "already bounds-checked across the interval `[delta + lo_extent, delta + hi_extent]`"; `lo_extent == hi_extent == 0` degenerates to the old `PtrAdd`. B4's `lir_postpone.rs` no longer emits two `PtrAdd(lo) / PtrAdd(hi)` probes — they collapse into a single `PtrAddChecked`. `CellAddAt.off` / `CellSetAt.off` widened from `i8` to `isize`; the emitter picks the disp8 vs disp32 form automatically (`src/backend/asm.rs::{AddMem8ImmDisp32, MovMem8ImmDisp32}`, `src/backend/x86_64/encode.rs::emit_mem8_disp32`). The codegen backend in `src/backend/codegen.rs` maintains a "verified window" state machine: `CellAdd*` / `CellSet*` / `ZeroRun` are transparent to r13, `PtrAdd` inside the window skips the bounds check, and `Label` / `Jump*` / `Scan*` / `LinearMul` / `PutByte` / `GetByte` clear it. C1 peephole rules were extended to merge / absorb `PtrAddChecked` (delta addition, interval union).
  - Files: `src/ir/lir.rs`, `src/ir/lir_postpone.rs`, `src/ir/lir_opt.rs`, `src/backend/codegen.rs`, `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`
- **C3 Displacement-form lowering** · **[landed]**: two new LIR variants `LirInst::CellAddAt { off, delta }` / `CellSetAt { off, val }` (`src/ir/lir.rs`) carry B4's flush output; the backend in `src/backend/codegen.rs` translates them to `AsmInst::AddMem8ImmDisp8` / `MovMem8ImmDisp8` (`src/backend/asm.rs`), encoded by `emit_mem8_disp8` in `src/backend/x86_64/encode.rs` as `add byte [r13 + disp8], imm8` / `mov byte [r13 + disp8], imm8` (ModRM mod=01; with R13 as base, `(rm & 7) == 5` avoids the SIB ambiguity, yielding machine code `49 80 45 <disp> <imm>` / `49 C6 45 <disp> <imm>`). `off == 0` is guarded by a `debug_assert!` in codegen — B4 canonicalises it back to `CellAdd` / `CellSet` (preserving D1's inc/dec short form); disp is enforced via `i8::try_from` to sit in `[-128, 127]`. disp32 is deferred until C2 lands — reason: offsets beyond disp8 must be paired with a range bounds-check or `ensure_tape_contains_r15`'s point-check semantics would be broken. The LIR peephole (`src/ir/lir_opt.rs`) gained 4 same-offset fold rules (`CellAddAt;CellAddAt`, `CellSetAt;CellAddAt`, `CellSetAt;CellSetAt`, `CellAddAt;CellSetAt`); cross-offset pairs never merge.
  - Files: `src/ir/lir.rs`, `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/codegen.rs`, `src/ir/lir_opt.rs`
- **C4 Propagate size hints to Scan / Zero** · **[landed]**: `src/ir/lir_scan_hint.rs`'s `promote_scan_hints` reuses the same "verified window" state machine as the C2 backend to identify bounds-check coverage still live at each `Scan` — when the window has positive extent in the `Scan`'s direction, the op is promoted to `LirInst::ScanWithHint { dir, hint_bytes }`; otherwise the bare `Scan` is retained and codegen falls back to the slow path. Inside a `ScanWithHint` loop the backend iterates with `inc r13` / `dec r13` (1 byte, no compare) and emits one full `PtrAddChecked` calibration on exit. Contiguous `Zero` runs are fused by the C1 peephole into `LirInst::ZeroRun { start: i32, count: u32 }` (disp32 form); until D2 lands the backend still zeros byte-by-byte, but the LIR shape is in place.
  - Files: `src/ir/lir.rs`, `src/ir/lir_scan_hint.rs` (new, 12 unit tests), `src/ir/lir_opt.rs`, `src/ir/lower.rs`, `src/backend/codegen.rs`

**Dependencies: C1 is independent; C2 depends on A2's range; C3 depends on B4; C4 depends on C2.**

### Phase D — Backend / codegen (depends on the semantic guarantees of Phase C)

- **D1 Instruction selection** · **[landed]**: `add/and/cmp r, imm` immediates automatically pick between `0x83 + imm8` (4 bytes) and `0x81 + imm32` (7 bytes), and long Jcc / JMP are iteratively shrunk to rel8 short jumps by a dedicated relaxation pass (`src/backend/x86_64/relax.rs`) — both live in the encode/relax layer and cover both platforms. The `CellAdd(±1) → inc / dec` (opcode `0xFE`) selection is now aligned across both backends: Linux `compile_lir_to_asm` (`src/backend/codegen.rs:382-395`) and Windows `compile_lir_to_windows_program` (`src/backend/x86_64/windows.rs:373-396`) both use the same `match imm { 0 => {}, 1 => IncMem8, 255 => DecMem8, _ => AddMem8Imm8 }` pattern, so every `+` / `-` becomes a 4-byte short form. A new cross-backend parity test (`src/backend/parity_tests.rs`) locks in the invariant and also covers `CellSet`, `CellAddAt`, `CellSetAt`, and modular-zero edge cases — a foundation that future phases will keep extending.
  - Files: `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/x86_64/relax.rs`, `src/backend/codegen.rs`, `src/backend/x86_64/windows.rs`, `src/backend/parity_tests.rs` (new, 7 unit tests)
- **D2 SIMD-specialized forms**:
  - **D2a `ScanWithHint(±1, hint_bytes)` → `repne scasb`** · **[landed, symmetric on both backends]**: C4-promoted `ScanWithHint`, when `hint > 0`, emits `xor eax, eax / mov rdi, r13 / mov rcx, hint_bytes / [cld or std] / repne scasb / [for backward dir: cld to restore DF=0]`, then `mov r13, rdi; add r13, -step` to back up the post-incremented rdi by one. Control then unconditionally falls through to the existing slow_top (`cmp [r13], 0; jz done; ptr_add(step); jmp slow_top`). On a hit, slow_top's first compare fires `jz done` immediately; on rcx exhaustion, r13 is at the last bounds-checked cell and slow_top's subsequent ptr_add lands on the boundary cell (handed to `emit_ptr_add_out`'s growth path). New `AsmInst::{Std, RepneScasb}` in the shared encoding layer (`asm.rs`, `encode.rs` byte-level tests, `debug.rs`); Linux and Windows codegen mirror each other. Bare `Scan(dir)` (no hint) keeps the slow loop because an unbounded rcx would risk reading past the tape.
  - **D2b Contiguous `ZeroRun(count ≥ 16)` → `rep stosb`** · **[landed, symmetric on both backends]**: a single `MovMem8ImmDisp8` is 5 bytes so the scalar form costs 5 N bytes; the SIMD setup (`xor eax, eax` 2 B + `lea rdi, [r13+start]` 4-7 B + `mov ecx, count` 5 B + `cld` 1 B + `rep stosb` 2 B) is ~14-17 B regardless of N — the exact crossover lands at `count == 16`. New `AsmInst::{XorEaxEax, MovEcxImm32, RepStosb}` with byte-level encoder tests (`31 C0`, `B9 + LE imm32`, `F3 AA`); both backends branch on the same threshold and `r13` is unchanged in either path (SIMD writes through `rdi`), so the verified window survives. Win64 ABI has no special concerns — `rep stosb` only clobbers the volatile `al`/`rdi`/`rcx`.
  - **D2c `LinearMul` ±1 columns** · **[landed, symmetric on both backends]**: `bl` already holds the head byte (loaded by `MovzxEbxFromMemR13`); factor==1 emits `AddMemR13Bl` (`41 00 5D 00`) directly, and factor==-1 (mod 256 == 255) emits `SubMemR13Bl` (`41 28 5D 00`) — 8-bit wraparound makes `cell + 255*bl ≡ cell - bl`, so the negative column also skips the imul. Both new `AsmInst` variants live in the shared encoding layer with byte-level unit tests; non-±1 columns keep the `MovEaxEbx; ImulEaxEbxImm32; AddMemR13Al` triple. Each ±1 column saves 8 bytes, a meaningful win on hot copy-loops like `[->+<]` / `[->-<]`. **Note**: the Win64 LinearMul body still differs from Linux by an `AddRegImm32(Rsp, ±8)` alignment cushion (introduced in P2-d, intentional ABI divergence); the per-column ±1 fast path itself is byte-equal on both sides, locked in by parallel `add/sub_mem_r13_bl_*` unit tests rather than the cross-backend parity harness.
  - **D2d `LinearMul` / `LinearMulWithSets` batched bounds-check + displacement writes** · **[landed, symmetric on both backends]**: the previous implementation issued two `emit_ptr_add_out` calls per factor column (forward + back), each ~12 x86 instructions for bounds checking + pointer movement. The new implementation, when all factor/sets offsets fit in `[-128, 127]` (disp8 range), performs a single `emit_ptr_add_checked_out(delta=0, lo=min_off, hi=max_off)` for batched bounds checking, then uses displacement-form writes (`AddMemR13BlDisp8` / `SubMemR13BlDisp8` / `AddMemR13AlDisp8` / `MovMem8ImmDisp8`) to operate on target cells directly without moving `r13`. Offsets outside disp8 range fall back to the per-factor path. The batched check establishes `verified_window = Some((lo, hi))`, allowing subsequent instructions to skip redundant bounds checks. Three new `AsmInst` variants (`AddMemR13BlDisp8(i8)` / `SubMemR13BlDisp8(i8)` / `AddMemR13AlDisp8(i8)`) with byte-level encoder tests + debug disassembly. A 3-factor `LinearMul` drops from ~72 instructions to ~20.
    - Files: `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/x86_64/debug.rs`, `src/backend/codegen.rs`, `src/backend/x86_64/windows.rs`
- **D3 Buffered I/O** · **[landed]**:
  - **Interpreter side**: `src/runtime/io.rs::BufferedStdIo` wraps process stdio with a 4 KiB `BufWriter<Stdout>` + `BufReader<Stdin>`. The `RuntimeIo` trait gained a default `flush()` method (no-op); `BufferedStdIo::flush` routes `BufWriter::flush` errors through `IoError::WriteError → RuntimeError::Io` so a late-flush failure surfaces as a runtime error instead of being silently swallowed in `Drop`. `Interpreter::run()` explicitly calls `io.flush()?` at program end, ordered "exec error wins, flush error reported only on exec success". The CLI `run_interpret` path constructs `BufferedStdIo::new()` by default.
  - **Linux backend side**: `src/backend/codegen.rs` adds `emit_init_output_buffer` (mmaps a 4 KiB anonymous region, pins `Rbx = buffer_base = write_ptr` and `Rbp = buffer_base + 4096 = end sentinel`) and `emit_flush_output` (helper subroutine that does `lea rsi, [rbp - 4096]` to recover the base, emits `write(1, base, rbx - base)`, and resets `rbx`). `PutByte` drops from a 5-instruction inline syscall (~42 bytes) to `mov al, [r13]; mov [rbx], al; add rbx, 1; cmp rbx, rbp; jne skip; call flush_output; skip:` (~20 bytes on the hot path, `write` syscall fires only once per 4096 bytes). `GetByte` flushes first so interactive prompts reach stdout before `,` potentially blocks on stdin; `exit(0)` flushes before the exit syscall. New `AsmInst::{MovAlMemR13, MovMemRbxAl}` variants + encoders with three encoding unit tests; `Reg64::Rbp` picks up its first use in this backend and is added to reg_num / Display.
  - **Windows backend side**: `src/backend/x86_64/windows.rs` mirrors the Linux register convention (`Rbx` = write pointer, `Rbp` = base + 4096 sentinel) but swaps the lower-level calls for their Win64 equivalents: the buffer is allocated with `VirtualAlloc(NULL, 4096, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE)` (`emit_init_output_buffer`), and `emit_flush_output` reserves an 88-byte Win64 frame, zeroes the `OVERLAPPED_SLOT_DISP` / `IO_COUNT_SLOT_DISP` slots, then issues `WriteFile(rdi=stdout, rdx=base, r8=count, r9=&BytesWritten)` — short writes are ignored, matching Linux. `PutByte` uses the same 5-instruction buffered hot path instead of per-byte WriteFile; `GetByte` and the normal exit path each gain one `Call(flush_output_label)` (the `exit_one` error path skips the flush so a fault doesn't mask the primary error). The `LinearMul` arm wraps the body in `Push(Rbx) + AddRegImm32(Rsp, -8)` ... `AddRegImm32(Rsp, 8) + Pop(Rbx)`: a bare `push rbx` would leave Rsp 8-byte-aligned at the inner `call ensure_tape` site, violating the Win64 16-byte-alignment requirement (Linux gets away with bare push/pop because `syscall` has no such constraint). Six new windows.rs unit tests plus one `#[cfg(target_os = "windows")]`-gated 5 KiB compile test in `tests/buffered_io.rs`.
  - Files: `src/runtime/io.rs`, `src/driver/run.rs`, `src/interp/engine.rs`, `src/backend/asm.rs`, `src/backend/codegen.rs`, `src/backend/x86_64/encode.rs`, `src/backend/x86_64/debug.rs`, `src/backend/x86_64/windows.rs`; tests `tests/buffered_io.rs` (interpreter + Linux + Windows compile, all three `>4 KiB`, plus `,` EOF returns 255), three encoding tests in `src/backend/x86_64/encode.rs`, six windows.rs unit tests
- **D4 Redundant mov elimination** · **[landed, symmetric on both backends]**: `LinearMul` non-±1 columns previously emitted `mov eax, ebx; imul eax, ebx, imm32; add [r13+disp], al`, but `imul eax, ebx, imm32` already reads from `ebx` and writes to `eax`, making the preceding `mov eax, ebx` completely dead. Removed, saving 2 bytes per non-±1 column. The `AsmInst::MovEaxEbx` variant has been deleted from asm.rs / encode.rs / debug.rs.
  - Files: `src/backend/codegen.rs`, `src/backend/x86_64/windows.rs`, `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/x86_64/debug.rs`
- **D5 Branch hints + loop-head 16B alignment** · **[landed, symmetric on both backends]**: `src/backend/x86_64/encode.rs` now prepends `0x2E` (not-taken hint, matching the not-taken semantics of BF `[` — we almost always enter the loop body) to every `Jz` / `JzShort`, and `0x3E` (taken hint, matching BF `]` which usually loops back) to every `Jnz` / `JnzShort`. The long form grows from 6 to 7 bytes and the short form from 2 to 3 bytes; `relax.rs` picks up a `short_form_len(inst)` helper so the rel8 offset calculation distinguishes hinted `JzShort` / `JnzShort` (3 bytes) from other short jumps (2 bytes). Intel has ignored these segment-override prefixes since Netburst, AMD still parses them as static branch hints, and both vendors treat the encoding as a legal no-op prefix so no microarchitecture regresses. 16B loop-head alignment: new `AsmInst::Align16` pseudo-instruction; codegen inserts `Align16` before every `Label` that is a `JumpIfNonZero` back-edge target (loop head). The encoder emits Intel-recommended multi-byte NOP sequences (1–8 byte NOP forms) based on the current offset; the relaxation pass automatically accounts for the variable size via `compute_offsets`. Both backends pre-scan `JumpIfNonZero` targets and insert symmetrically.
  - Files: `src/backend/asm.rs`, `src/backend/x86_64/encode.rs` (`emit_nop_sequence` + 2 tests), `src/backend/x86_64/debug.rs`, `src/backend/codegen.rs`, `src/backend/x86_64/windows.rs`, `src/backend/x86_64/relax.rs`
  - Files: `src/backend/x86_64/encode.rs`, `src/backend/x86_64/relax.rs` (`encoder_emits_three_bytes_for_hinted_short_jz` unit test)

**Dependencies: D1, D3, D4 are independent; D2 depends on C; D5 landed.**

### Phase E — Interpreter + runtime (mostly independent from A–D, parallelizable)

- **E1 Super-instruction lowering** · **[landed]**: added `src/interp/bytecode.rs` defining `InterpOp` (with fused forms `MoveAdd { d, k }` / `ZeroMove(d)`), `LinearMulPlan` (compact `Box<[(i32, i16)]>` factors shared via `Arc` to avoid repeated clones after O2 fixed-point duplication), and `InterpProgram`; added `src/interp/lower.rs::lower_hir_to_bytecode` performing HIR → InterpOp lowering in a single pass with `Move; Add → MoveAdd` / `Zero; Move → ZeroMove` fusion plus `Loop` → `LoopStart { end_pc } / LoopEnd { start_pc }` absolute-pc back-patching (scratch stack as `Vec<u32>`). `engine.rs::exec_bytecode` switched from the recursive `exec_block` to a flat pc-indexed dispatch loop; `[` / `]` become a single compare + absolute jump, no Rust frame per BF loop iteration.
  - Files: `src/interp/bytecode.rs` (new), `src/interp/lower.rs` (new, 13 unit tests), `src/interp/engine.rs`, `src/interp/mod.rs`
- **E2 Threaded dispatch** · **[landed]**: `InterpOp::tag()` returns a dense opcode index (implemented as a safe `match`, since `#![forbid(unsafe_code)]` rules out `mem::transmute` / repr punning); added `src/interp/handlers.rs` where 11 `fn(&mut Interpreter<I, H>, &InterpOp, usize) -> Result<usize, RuntimeError>` handlers feed `dispatch_table::<I, H>() -> [Handler<I, H>; INTERP_OP_TAG_COUNT]`. `engine.rs::exec_bytecode`'s monolithic `match` was replaced by `pc = table[op.tag()](self, op, pc)?` — one table load + one indirect call per op, giving the CPU's indirect-branch predictor per-opcode state instead of a single hot-block jump. Each handler `if let`s its variant and falls into a cold `unreachable!()` on mis-dispatch. Stable Rust has no sibling-tail-call, so handlers return the next pc through the main loop; per the plan, if LLVM fails to optimize the pattern the fallback is `match + #[inline(always)]`.
  - Files: `src/interp/bytecode.rs`, `src/interp/engine.rs`, `src/interp/handlers.rs` (new)
- **E3 Interpreter LinearMul fast path** · **[landed]**: `src/interp/handlers.rs::exec_linear_mul` short-circuits factor ±1 columns to skip the `wrapping_mul` / `rem_euclid` reduction (`delta = v as i32` / `delta = -(v as i32)`), saving two ALU ops per factor. All factors now go through a new `Tape::add_at(off, delta)` helper instead of the previous `move_ptr(off); add_current; move_ptr(-off)` triple — one grow check replaces two, and the virtual visit no longer inflates `move_left_units` / `move_right_units` in the tape-usage summary. `src/runtime/tape.rs` refactors the tape-growth logic into a shared `ensure_range(target)` used by both `move_ptr` and `add_at`; `ptr_min` / `ptr_max` / `right_grew_bytes` continue to track cells touched (virtual or real). Generalised SIMD (`rep stosb`, slice `memcpy`) is still deferred to D2 / a future interp SIMD pass.
  - Files: `src/runtime/tape.rs` (new `add_at` + 4 unit tests covering offset, wrap, bidirectional grow), `src/interp/handlers.rs`
- **E4 Tape backend restructuring** · **[landed, revised design]**: the original plan called for mmap + centered-copy, but that conflicts with `#![forbid(unsafe_code)]`. Revised: keep the current `Vec<u8>` left/right-spliced layout but switch to geometric doubling (`new_len = max(needed, old_len * 2)`, with the left half using an additional 8-byte floor due to its initial empty state): amortized O(1) per accessed cell, avoiding the O(n) resize that a single boundary-crossing step would otherwise trigger. `TapeStats::right_growth` was renamed to `right_grew_bytes` to clarify its semantics. Should a shared backend-tape call for a real mmap version later, it can be introduced under a runtime feature flag without breaking `forbid(unsafe)`.
- **E5 Criterion micro-benchmark suite** · **[landed]**: added `benches/`, using a subset of the [matslina standard benchmark suite](https://github.com/matslina/bfoptimization) — **factor.b**, **mandelbrot.b**, **hanoi.b**, **dbfi.b**, **long.b**, and **awib-0.4.b**. Measures O0 / O1 / O2 / O3 × (interpret, compile+run). These programs cover different contraction ratios (40%–75%) and different hot-loop patterns; they are the established BF-optimization-literature benchmarks. Reference speedup ranges from the literature: hanoi.b ≈ 130×, mandelbrot.b several tens of times, awib-0.4 ≈ 2.4× (full opt vs no opt). Serves as the regression baseline for all A–D passes.

> **E5 should land first**: the payoff of every subsequent optimization must be quantified through it.

**Dependencies: E5 has none, first to land; E1 → E2; E3 has none; E4 and D3's I/O rework can share a milestone.**

### Phase F — Long-term goals

- **F1 JIT execution**:
  - **F1a Whole-program JIT** → see H1 (landed).
  - **F1b Tiered JIT (interpreter-driven hot-spot compilation)** → see H3 (P0 / P1 / P2 landed, Linux x86_64 only).
- **F5 Incremental compilation cache**: cache HIR / LIR / obj for a fixed `.bf` source keyed by content hash. Only worth it if E5 shows compile time is a significant fraction of total time.

### Phase G — Backend refactoring (landed)

> Goal: eliminate code duplication between the Linux / Windows backends.

- **G1 Extract shared codegen logic** ✓: created `src/backend/codegen_common.rs`, containing:
  - `LabelAllocator`: unified internal label allocator (replacing Linux's `fresh_internal_label(&mut u32)` and Windows's `LabelAllocator::new()`).
  - 6 utility functions: `map_label`, `mem8_add_at_r13`, `mem8_set_at_r13`, `emit_add_reg_isize`, `emit_ptr_add_out`, `emit_ptr_add_checked_out`.
  - `PlatformEmitter` trait: abstracts `emit_put_byte` / `emit_get_byte` / `needs_rsp_alignment()` (the three ABI-specific hooks).
  - `emit_lir_body()`: complete LIR→AsmInst translation loop covering all ABI-neutral match arms (PtrAdd, PtrAddChecked, CellAdd, CellSet, CellAddAt, CellSetAt, ZeroRun, LinearMul, LinearMulWithSets, Scan, ScanWithHint, Label, JumpIfZero, JumpIfNonZero), delegating PutByte/GetByte to `PlatformEmitter` callbacks and parameterizing LinearMul's Win64 RSP alignment via `needs_rsp_alignment`.
  - Effect: `codegen.rs` 2120→1532 lines (−588), `windows.rs` 1655→1221 lines (−434), `codegen_common.rs` 516 lines, net −506 lines.

### Phase H — JIT evolution (partially landed)

> Goal: evolve from AOT-only to JIT execution, ultimately enabling interpreter-driven hot-spot compilation.

- **H1 Whole-program JIT** · **[landed]**: `-m jit` mode (Linux x86_64 only). `src/driver/run.rs::run_jit` reuses `compile_linux_asm` → `relax_jumps` → `encode_program` to produce complete x86_64 machine code; `amazingbf_jit::JitBuffer::new` (`crates/jit/src/lib.rs`) performs `mmap(RW)` → `copy` → `mprotect(RX)` (W^X flip) via raw syscalls; `execute()` jumps in via `transmute`. The JIT crate uses `#![deny(unsafe_code)]` with targeted `#[allow]` exemptions for mmap/mprotect/munmap/transmute. The generated code currently terminates via `syscall(SYS_exit, 0)`, so the host process exits — control cannot return to the caller (H2 addresses this).
  - Files: `crates/jit/Cargo.toml`, `crates/jit/src/lib.rs` (3 unit tests with fork+waitpid verification), `src/driver/run.rs`, `src/driver/config.rs` (`RunMode::Jit`), `src/error.rs` (`Error::Jit`), `src/cli.rs` (`-m jit` help text), `tests/jit_pipeline.rs` (6 integration tests: 5 cases + 1 all-opt-level cross)
- **H2 ret-based JIT execution** · **[landed]**: the JIT-generated code now uses `ret`-based return instead of `exit(0)` termination, allowing the host process to continue after JIT execution. Implementation:
  - `compile_lir_to_jit_asm()` in `codegen.rs` generates a SysV ABI function: prologue saves callee-saved registers (rbp, rbx, r12–r15), receives tape state from arguments (rdi=tape_base, rsi=data_ptr, rdx=tape_end), and the epilogue restores registers + `ret` with eax=0 (success) or eax=1 (error).
  - `JitBuffer::execute_fn(tape_base, data_ptr, tape_end) -> i32` in `crates/jit/src/lib.rs` calls the JIT code as a typed function pointer.
  - `JitTape` struct in the JIT crate encapsulates mmap allocation and provides safe `base()`/`data_ptr()`/`end()` accessors, keeping the main crate `#![forbid(unsafe_code)]`-clean.
  - `run_jit()` in `run.rs` allocates a `JitTape`, passes it to `execute_fn`, and checks the return code. O3 special cases (trivial exit, precomputed stdout) still use the H1 `execute()` path.
  - Files: `crates/jit/src/lib.rs` (`execute_fn`, `JitTape`, 2 new unit tests), `src/backend/codegen.rs` (`compile_lir_to_jit_asm`), `src/driver/run.rs` (`run_jit` rewrite)
- **H3 Tiered JIT (interpreter-driven hot-spot compilation)** · **[P0 + P1 + P2 landed, Linux x86_64 only]**:
  - **H3-P0 Trip-count profiling** · **[landed]**: `LoopProfile` in `src/interp/profile.rs` (a `Vec<u64>` indexed by `LoopStart` pc) and `Interpreter::enable_profiling` (allocates only when explicitly enabled, zero overhead otherwise); `handle_loop_end`'s back-edge path calls `record_back_edge`.
  - **H3-P1 Compilation pipeline + tape bridging** · **[landed]**: `src/interp/jit_compile.rs::compile_hot_loop(body)` lowers an `InterpOp` slice to LIR (recursing through nested loop structures), then runs `relax_jumps` + `encode_program` + `JitBuffer::new`; `src/runtime/tape.rs::snapshot_flat / restore_from_flat` bridge between the split tape and a flat buffer; `crates/jit/src/lib.rs::JitTape::from_slice / as_slice / data_ptr_at` give the main crate an mmap'd bridging buffer.
  - **H3-P2 Inline OSR + end-to-end integration** · **[landed]**:
    - Candidate filter `analyse_eligibility(body) -> Option<(min_off, max_off)>` requires balanced (cumulative ptr delta = 0) + bounded reach (every Move/MoveAdd/ZeroMove plus LinearMul* factor/sets offsets stay within a finite window) + no top-level nested loop / Scan / I/O. Covers hanoi.b's `Move(1) Zero Move(-1) Add(-1)` (95.30% of iterations) and long.b's `LinearMulWithSets` shape (94.17% of iterations); mandelbrot's `Unbalanced` hot loops are rejected outright (consistent with the B7-β investigation).
    - Trigger: `handle_loop_end`'s back-edge path checks `trip_count >= threshold` and on first crossing calls `compile_jit_slot` to run `analyse_eligibility` + `compile_hot_loop` exactly once and write the result into `Interpreter::jit_cache` (`HashMap<u32, JitSlot>`; `JitSlot::Failed` is sticky, never retried). The same back-edge and every subsequent one go through `dispatch_jit`: pre-grow the tape to cover `(min_off, max_off)`, `snapshot_flat`, `JitTape::from_slice`, `execute_fn`, `restore_from_flat`, then jump past `]`. The balanced contract guarantees the JIT exits with the same `data_ptr_offset`, so restore can reuse the entry offset.
    - Tiered-specific JIT codegen `compile_lir_to_jit_loop_asm` shares H2's ABI but skips `emit_init_output_buffer` (eligibility guarantees no I/O in hot loops) and `flush_output`, avoiding a 4 KiB output-buffer leak per dispatch.
    - CLI: new `RunMode::Tiered` + `--jit-threshold N` (default 10000); `-m tiered` runs `run_tiered`, exposed only on Linux.
    - Files: `src/interp/profile.rs`, `src/interp/jit_compile.rs` (`analyse_eligibility` + 9 unit tests), `src/interp/engine.rs` (`JitSlot` / `program: Arc<InterpProgram>` / `enable_tiered_jit`), `src/interp/handlers.rs` (`compile_jit_slot` + `dispatch_jit`), `src/runtime/tape.rs`, `src/backend/codegen.rs` (`compile_lir_to_jit_loop_asm`), `src/driver/{config,run}.rs`, `src/cli.rs`, `crates/jit/src/lib.rs` (`from_slice` / `as_slice` / `data_ptr_at` + 3 unit tests), `tests/tiered_jit_pipeline.rs` (9 integration tests covering 8 cases + a high-threshold no-JIT regression check), `benches/standard_suite.rs` / `benches/compile_levels.rs` add a `tiered` column.
    - **Known v1 limitations (deliberately deferred)**:
      - Unbalanced loops (would require an ABI out-param to return the final `data_ptr_offset`), JIT-side tape growth (sidestepped via pre-grow in v1), and top-level nested loops are all rejected. P3 may relax the nesting restriction for specific shapes.
      - Linux x86_64 only; Windows continues to be `#[cfg]`-gated as in P0/P1.
  - **H3-P2 Step 2A — `Vec<JitState>` + Failed short-circuit** · **[landed]**: the first round of bench numbers exposed two regressions (mandelbrot.b -O3 at 4.2× slower than interpret, long.b -O3 at 1.5× slower) caused by `HashMap<u32, JitSlot>::contains_key` running on every back-edge. Fix: replace the HashMap with `Vec<JitState>` indexed directly by `LoopStart` pc; short-circuit `Failed` slots; move `record_back_edge` into the `Cold` arm so once a slot is decided the trip-counter stops mutating. Result: mandelbrot 32.8 s → 14.1 s (4.2× → 1.6×), long 216 ms → 149 ms (parity with interpret).
  - **H3-P2 Step 2B — Persistent JIT scratch buffer** · **[landed]**: each dispatch still mmap'd a fresh `JitTape` + memcpy in/out, paying ~10 µs of syscall overhead per call. Fix: `Interpreter::jit_scratch: Option<JitTape>` reused across dispatches and grown lazily; new `JitTape::as_mut_slice` + `Tape::snapshot_flat_into` let the dispatch path memcpy directly with no intermediate `Vec`. Result: mandelbrot 14.1 s → 11.0 s (1.6× → 1.3×), long stays at parity (149 → 152 ms within noise).
    - Files: `src/interp/engine.rs` (`JitState`, `jit_scratch`), `src/interp/handlers.rs` (dispatch rewrite, `Failed` short-circuit), `src/runtime/tape.rs` (`snapshot_flat_into` / `flat_required_bytes`), `crates/jit/src/lib.rs` (`as_mut_slice`)

  - **H3 v2 baseline (matslina suite, 2026-04-29, commit 105a0da)**:
    | Workload | mode | O0 | O1 | O2 | O3 |
    |---|---|---|---|---|---|
    | long | interp | n/a | 150 | 150 | 150 ms |
    |  | exec | 1.38 s | 15.8 | 15.0 | **273 µs** |
    |  | jit | 1.80 s | 15.4 | 16.3 | 153 ms |
    |  | tiered | n/a | 152 | 152 | 153 ms |
    | dbfi | interp | 21.4 s | 11.4 | 11.4 | 11.4 s |
    |  | exec | 1.92 s | 1.85 | 1.85 | **1.85 s** |
    |  | jit | 1.95 s | 1.73 | 1.73 | 1.73 s |
    |  | tiered | n/a | 12.10 | 12.10 | 12.10 s |
    | factor | interp | n/a | n/a | n/a | 1.72 s |
    |  | exec | 385 | 110.7 | 110.6 | **110.9 ms** |
    |  | jit | 390 | 111.3 | 111.9 | 112.0 ms |
    |  | tiered | n/a | 1.77 | 1.78 | 1.77 s |
    | mandelbrot | interp | n/a | n/a | n/a | 8.56 s |
    |  | exec | n/a | 812 | 813 | **270 µs** |
    |  | jit | n/a | 742 | 743 | 8.76 s |
    |  | tiered | n/a | n/a | n/a | 10.97 s |
    | hanoi | exec | n/a | n/a | 5.81 ms | **330 µs** |
    |  | jit | n/a | n/a | 31.4 | 55.8 ms |
    |  | tiered | n/a | n/a | 56.1 | 55.8 ms |

  - **H3 ROI assessment — tiered v2 yields no positive value across all five workloads, ranging from +1% (long-O3) to +28% (mandelbrot-O3) regression vs. plain interpret.** Why:
    - **Workloads where eligibility passes (long / hanoi / dbfi / factor)**: B7-α has already folded the hot loop into a single `LinearMulWithSets`; the interpreter's tagged-dispatch + native handler runs each outer iteration in ~50 ns. JIT'ing that single op only drops it to ~30 ns, and the per-dispatch bridging fixed cost (memcpy + function call) eats the savings.
    - **Workloads where eligibility fails (mandelbrot)**: every hot loop is `Unbalanced`, so nothing JITs. The remaining ~28% regression is `interp.jit_enabled` check + `jit_cache[idx]` read + tag match, multiplied by ~10⁹ back-edges.
    - **`-m jit` -O3 being slow on mandelbrot/long**: the O3 fold path runs the interpreter offline first to compute stdout, then emits a trivial write+exit ELF. So jit/O3's wall-clock ≈ interpret time. `-m compile -O3` moves that step out of the measurement loop, which is why exec/O3 reports microseconds.
  - **H3-P3 (persistent mmap-Tape backend, originally the "main perf axis") deferred**: under this baseline, even zero-cost dispatch bridging would only narrow the per-iteration JIT vs. interpreter gap from 30 ns vs. 50 ns. The matslina-suite ceiling for P3 is ~30% (150 ms → ~100 ms range), out of proportion to the engineering cost.
  - **Next-step candidates (ROI-ordered)**:
    1. **H3 eligibility relaxation (top-level `Scan(±1)` + Unbalanced loops)**: mandelbrot's hot loops are all Unbalanced; adding an ABI out-param to return the final `data_ptr_offset` lets them JIT. Reach: mandelbrot interpret 8.56 s → exec 812 ms (i.e. -O1 native), so the upper bound is ~10×; realistic gain is 3–5× (~2-3 s) given outer-loop reuse limits. **Most likely source of a substantive win.**
    2. **F5 incremental compile cache**: bench shows compile time isn't the bottleneck, weak ROI.

**Dependencies: H1, H2, H3 (P0+P1+P2 + Step 2A/2B) landed. The tiered JIT baseline shows v2 at parity with interpret; the next substantive win requires eligibility relaxation.**

---

## 3. Dependency Overview

```
E5 (bench)  ──────────────────────────────────┐
                                              ▼
A1 → A2 → A3 → A4                        (regression baseline)
  │    │    │    │
  │    │    │    └→ B1 (DSE) ✓
  │    │    └→ B2 (zero-loop) ✓, B5 (LICM) ✓, B6 (unroll) ✓
  │    └→ B3 (LinearMul generalization) ✓, B4 (pointer postponement) ✓, B7-α (K6 simplified) ✓
  │         │
  │         └→ C3 (displacement) ✓ → D2 ✓ / D4 ✓
  │
  └→ C2 (bounds batching) ✓ → C4 (scan hint) ✓, D2 (SIMD) ✓

C1 (LIR peephole) ✓, D1 (instruction selection) ✓,
D3 (buffered I/O — interpreter ✓ / Linux backend ✓ / Windows backend ✓),
E1 / E2 (super-instructions + threaded dispatch) ✓,
E3 (interp LinearMul ±1 fast path) ✓, E4 (tape doubling) ✓ can all start in parallel.
Landed: E5, C1, D1, E4, Phase A (A1–A4), B1, B2, B3, B4, B5, B6, B7-α (P1+P2+P3), C2, C3, C4, E1, E2, E3, D2 (a/b/c/d, symmetric on both backends), D3 (interpreter + Linux backend + Windows backend), D4 (redundant mov elimination), D5 (branch hints + loop-head 16B alignment), G1 (backend refactoring), H1 (whole-program JIT), H2 (ret-based JIT), H3-P0 / H3-P1 / H3-P2 (tiered JIT v1: balanced + bounded reach + no top-level nesting, Linux x86_64).
Investigated and deferred: B7-β (ROI <2%, requires full K6 algorithm).

Phase H / F dependency chain:
H1 (landed) ──→ H2 (landed) ──→ H3-P0 / P1 / P2 (landed)
                      │                      │
                      └──→ JIT benchmark E5   └──→ H3-P3 (persistent shared tape, perf tuning)

F5 (incremental cache) is outside the near-term dependency graph.
```

---

## 4. Files of Interest

| Layer | File | Phase |
|---|---|---|
| HIR | `src/ir/hir.rs` | B3 / B4 if new variants needed |
| HIR | `src/ir/optimize.rs` | Main edit point for Phases A / B |
| HIR | `src/ir/analysis/` | A1–A4 skeletons (landed) |
| HIR | `src/ir/dse.rs` | B1 DSE (landed) |
| HIR | `src/ir/loop_stats.rs`, `src/ir/loop_profile.rs` | B7 Phase 0 measurement (landed) |
| LIR | `src/ir/lir.rs`, `src/ir/lower.rs` | B / C may add `PtrAddChecked` / `CellAddAt` |
| LIR | `src/ir/lir_opt.rs` (new) | Main venue for Phase C |
| Backend | `src/backend/codegen.rs`, `src/backend/codegen_common.rs`, `src/backend/x86_64/encode.rs`, `src/backend/asm.rs` | Phase D / G1 / H2 |
| Backend | `src/backend/x86_64/elf.rs`, `src/backend/x86_64/windows.rs` | D3 buffered I/O / G1 |
| JIT | `crates/jit/src/lib.rs` | H1 (landed) / H2 |
| Runtime | `src/interp/engine.rs`, `src/runtime/{tape,io,host}.rs` | Phase E / F1b |
| Bench | `benches/` (new) | E5 |
| Tests | `tests/cases_pipeline.rs`, `tests/compile_artifacts.rs`, `tests/jit_pipeline.rs` | Regression after each phase |

---

## 5. Verification

- Before merging each TODO: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
- Correctness regression: `tests/cases_pipeline.rs`, `tests/bfsc_pipeline.rs`, `tests/windows_target.rs` must stay green
- Performance regression: once E5 lands, every Phase B / C / D pass must submit a criterion comparison (factor / mandelbrot / hanoi / dbfi / long / awib-0.4 suite, interpret + compile variants)
- Slow benchmark: `cargo bench --bench compile_levels` pre/post comparison
- Binary size: for D1 / D2 record the compiled `.text` byte-size change

---

## 6. Non-goals

- Do not adjust the semantics of the existing O0–O3 CLI flags (no changes to `src/cli.rs` / `src/driver/config.rs`); new optimizations fit into the existing levels.
- No third-party runtime dependencies (stay `std`-only).
- Do not break the main crate's `#![forbid(unsafe_code)]`; the JIT crate (`crates/jit`) uses `#![deny(unsafe_code)]` with targeted `#[allow]` exemptions for mmap/mprotect/munmap/transmute — this boundary was established in H1. Phase F1b's tiered JIT will follow the same isolation pattern.
- Both language editions (`docs/OPTIMIZATION_PLAN.md` and `docs/OPTIMIZATION_PLAN_CN.md`) are updated in lockstep.

---

## 7. References / Prior Art

The key techniques in this roadmap are not novel; the following is the industry provenance and reference implementations for each TODO, useful when implementing.

### Core surveys

- **[matslina, "Brainfuck Optimization Strategies"](http://calmerthanyouare.org/2015/01/07/optimizing-brainfuck.html)** — the foundational text for the BF-optimization field. Defines the modern BF-compiler optimization pipeline: contraction → clear loops → copy/multiply loops → operation offsets → scan loops. The companion repo [matslina/bfoptimization](https://github.com/matslina/bfoptimization) provides reference implementations and benchmark data.
- **[Nayuki, "Optimizing Brainfuck Compiler"](https://www.nayuki.io/page/optimizing-brainfuck-compiler)** — gives the crispest formal definitions for pointer postponement and balanced-loop classification; B4 / C3 semantics track this paper directly.

### Reference implementations (ordered by relevance to this roadmap)

- **[Wilfred/bfc](https://github.com/Wilfred/bfc) (Rust)** — "industrial-grade" positioning, with a position-preserving IR and idempotence / observational-equivalence regression tests for optimizations. Its pass list (fusing increments, fusing movements, fusing movements into adds, postponing movements, simple loops, assign followed by add, complex loops) maps almost one-to-one onto Phases B–C of this roadmap; the closest Rust-ecosystem analogue.
- **[matslina/awib](https://github.com/matslina/awib)** — a BF compiler written in BF, with 6 backends. Useful reference for codegen choices and benchmark comparisons.
- **[jjcmoon/Oizys](https://github.com/jjcmoon/Oizys)** — proposes the K6 algorithm (source of B7), handling deeply nested balanced loops.
- **[Rodrigodd's BF-compiler trilogy](https://rodrigodd.github.io/2022/10/21/bf_compiler-part1.html)** — Part 1 optimizing interpreter, Part 2 single-pass JIT, Part 3 Cranelift JIT. Engineering references for Phase E (interpreter) and Phase F1 (JIT).
- **[danthedaniel/BF-JIT](https://github.com/danthedaniel/BF-JIT) (Rust)** — hybrid AOT + JIT compilation: small loops compile immediately, hot loops compile lazily. A concrete reference for the F1 tiered-JIT strategy.
- **[Brian Quinlan's brainfuck-jit](https://github.com/brianquinlan/brainfuck-jit)** — a minimal example of operation-offsets lowered directly to x86-64.

### Full compiler list

- **[Esolang: Brainfuck implementations](https://esolangs.org/wiki/Brainfuck_implementations)** — a community-maintained panoramic list, including Tritium / libbf / esotope-bfc / ssbi / Hamster / bfcfs and other implementations worth cross-referencing but not individually listed in this roadmap.

### Benchmark program sources

- factor.b / hanoi.b / mandelbrot.b / dbfi.b / long.b: from the [matslina/bfoptimization](https://github.com/matslina/bfoptimization) companion.
- awib-0.4.b: from [matslina/awib](https://github.com/matslina/awib), used as a self-benchmark input.

These programs recur across the existing literature, so using them makes this toolchain's optimization effects directly comparable against bfc / awib / Oizys / Tritium, rather than having to build an incommensurable in-house benchmark set.

---

## 8. Related Subsystems (outside the optimization roadmap scope)

The following subsystems benefit from the optimization pipeline documented above, but their own development is not tracked in this document. Listed here for awareness only.

- **BFS (Brainf Script) compiler**: `src/bfsc/` (lexer → parser → typeck → codegen), `bfsc` binary entry point. Compiles high-level `.bfs` source to Brainfuck; optionally `-c` to go straight through `driver::run` and produce a native executable. Integration tests in `tests/cases_bfs/`.
- **Tauri GUI**: `src/gui.rs` + `src/runtime/gui_io.rs`, feature-gated (`gui`), `bf-gui` binary entry point. Runs the BF interpreter inside a Tauri window with screen-buffer output and keyboard input. Example programs in `examples/gui_*.bfs`.
- **JIT crate**: `crates/jit/` (workspace member `amazingbf-jit`), provides `JitBuffer` (mmap/mprotect/munmap + execute). Consumed by H1 whole-program JIT; H2 / F1b will extend its API.
