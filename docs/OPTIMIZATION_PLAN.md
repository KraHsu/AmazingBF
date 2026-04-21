# AmazingBF Compilation Optimization Modernization Plan

This document is the modernization roadmap for the AmazingBF toolchain's compilation optimizations. It first surveys the existing HIR / LIR / backend / interpreter optimization state, then lists the TODOs in implementation-dependency order, to serve as the reference for subsequent implementation tasks.

> Scope: HIR analysis + passes, a new LIR peephole layer, backend/codegen, interpreter + runtime, plus a section of long-term goals.
> Principles: preserve the O0–O3 CLI semantics; keep `#![forbid(unsafe_code)]`; stay `std`-only; do not cross layer boundaries.
> Bilingual: the Chinese companion lives at `docs/OPTIMIZATION_PLAN_CN.md` and is updated alongside this file.

---

## 1. Current State

### 1.1 HIR layer (`src/ir/hir.rs`, `src/ir/optimize.rs`)

- HIR variants: `Move(isize)` / `Add(i32)` / `PutByte` / `GetByte` / `Zero` / `LinearMul(Vec<(isize,i32)>)` / `Loop(Vec<HirInst>)`
- **O0** `optimize_o0` (`optimize.rs:38-42`): fuse adjacent `Move/Add`, single pass
- **O1** `optimize_o1` (`optimize.rs:60-68`): O0 fusion + `try_scan_loop` / `is_byte_clear_loop` / `try_linear_loop` (`optimize.rs:151-197`) + A1-`TapeState`-driven constant propagation (`optimize.rs:219-307`) + `push_o1` peephole (`optimize.rs:309-329`) + **B1 DSE** (`src/ir/dse.rs`) chained at the end of the pipeline
- **O2** `try_optimize_o2` (`optimize.rs:74-87`): fixed-point iteration of O1 (DSE included), capped at 4096 iterations
- **O3**: not a standalone HIR pass; `src/driver/run.rs:138-176` performs whole-program folding (no `PutByte` → `exit(0)`; no `GetByte` → run offline then `write + exit`)
- **Analysis infrastructure**: under `src/ir/analysis/`, all of A1 `TapeState` / A2 `LoopEffect` / A3 `CellLattice` four-point lattice / A4 `run_forward` forward dataflow skeleton are in place (57 unit tests). A1+A3 already drive O1's constant propagation (A3's `add_wrapping` powers the `Add` arm of `TapeState::apply`); A2 / A4 remain pending infrastructure, awaiting consumers like B2 / B3 / B5

### 1.2 LIR layer (`src/ir/lir.rs`, `src/ir/lower.rs`)

- LIR variants: `PtrAdd` / `CellAdd` / `CellSet` / `LinearMul` / `Scan` / `PutByte` / `GetByte` / `Label` / `JumpIfZero` / `JumpIfNonZero`
- `lower_to_lir_block` (`lower.rs:48-82`) is a pure mechanical lowering that only drops zero deltas
- **No peephole, no scheduling, no bounds-check aggregation**

### 1.3 Backend (`src/backend/codegen.rs`, `src/backend/x86_64/`)

- Fixed register assignment: `R12 = tape_base` / `R13 = data_ptr` / `R14 = tape_end` / `R15 = scratch` (`asm.rs:16-33`)
- Each `PtrAdd` ≈ 12-instruction fast path (`codegen.rs:78-107`); out-of-range cases call `ensure_tape_contains_r15` (`codegen.rs:442-563`)
- Each `.` / `,` maps to a `write` / `read` syscall or `WriteFile` / `ReadFile` (`codegen.rs:204-231`, `windows.rs:518-572`)
- All jumps are 5-byte `rel32`; no `inc` / `dec` selection; no SIMD; no codegen peephole

### 1.4 Interpreter + Runtime (`src/interp/engine.rs`, `src/runtime/`)

- Single-level `match` dispatch (`engine.rs:66-132`); no threaded dispatch, no super-instructions
- `Tape` uses a `Vec<u8>` with left/right halves spliced together (`tape.rs:34-209`), grown with `Vec::resize`; **inconsistent with the "mmap + doubling" wording in `CLAUDE.md`**
- Benchmark infrastructure (E5) landed: `benches/standard_suite.rs` (Criterion, interpret/execute the matslina suite) and `benches/compile_levels.rs` (custom harness, compile+run timings for `tests/cases/*.bf`). `tests/compile_artifacts.rs` only checks artifact correctness

---

## 2. Modernization Roadmap

Six phases in dependency order: A analysis infrastructure → B HIR passes → C new LIR peephole layer → D backend/codegen → E interpreter + runtime (parallelizable) → F long-term goals (not in the near-term dependency graph).

### Phase A — Analysis infrastructure (prerequisite for most other passes)

- **A1 Symbolic tape state** · **[landed]**: `src/ir/analysis/tape_state.rs`'s `TapeState` anchors at the block-entry `data_ptr`, using a `BTreeMap<isize, CellLattice>` to record the lattice value at every visited offset plus the current ptr offset. Includes `merge_in_place` (degrades to pessimistic when ptrs disagree) and `clobber_all` (triggered by non-I/O `Loop` / `Scan` / `LinearMul`). Wired into O1's `optimize_block_o1_with_parent_env` with `ConstPropXfer: Transfer<TapeState>` driving the symbolic execution.
- **A2 Pointer-delta abstract interpretation** · **[landed]**: `src/ir/analysis/loop_effect.rs`'s `LoopEffect::analyze` produces `{ net_ptr_delta, touched, reads_cell, writes_cell, has_io }`, with `pointer_delta_range` computing `(min_off, max_off, net_delta)`. Currently a skeleton; `try_linear_loop` has not yet been migrated to it (B3 will do that).
- **A3 Cell-value abstract lattice** · **[landed]**: `src/ir/analysis/lattice.rs`'s `CellLattice { Top, NonZero, Zero, Const(u8) }` provides `meet` / `add_wrapping` / `is_zero` / `is_nonzero` / `known_u8`. The `Add(k)` arm of `TapeState::apply` now goes through `current.add_wrapping(k)`, lifting the transfer from "literal-equivalence" to proper lattice semantics; `NonZero` is preserved in cross-block facts produced by `merge_in_place`.
- **A4 Cross-block dataflow framework** · **[landed]**: `src/ir/analysis/dataflow.rs` provides the `Fact` / `Transfer` traits and the `run_forward` driver; `transfer_loop` iterates the lattice to a fixed point with a 64-iter cap and fail-safes back to `Fact::bottom()`. Implementations for both `Option<TapeState>` and `TapeState` facts are in place. Currently `run_forward` is exercised only in unit tests; the first real consumer (live-cell analysis, paving the way for B5 LICM) awaits a later phase.

**Dependency: A1 → A2 → A3 → A4 are all ready; Phase B can unfold.**

### Phase B — HIR pass expansion (depends on Phase A)

- **B1 Dead store elimination** · **[landed]**: `src/ir/dse.rs`'s `dead_store_elimination` is a forward syntactic rewrite — virtual pointer + `BTreeMap<isize, usize>` pending-write set. A write at some offset is dropped when a later write at the same offset covers it with no intervening read. `Loop` / `Scan` / `LinearMul` act as barriers that clear pending and recurse into `Loop` body; `PutByte` / `Add` commit (read-back) prior writes; `GetByte` / `Zero` unconditionally overwrite prior writes; `GetByte` itself is never droppable (input side effect). Covers the two cases `push_o1` misses: Move intervals and GetByte overwrites. Chained at the end of `optimize_o1`, automatically amplified by the O2 fixed-point loop. Final implementation is purely syntactic and does not consume A4 `run_forward` (A3 `CellLattice` is reserved for later B2 / B5 cross-block variants).
  - Files: `src/ir/dse.rs`; integration point `src/ir/optimize.rs:optimize_o1`
- **B2 Cross-block extension of known-zero-cell loop elimination**: O1 only does this at the block-entry `ConstEnv`; A3 enables recognizing `value_at_ptr == Zero` at `Loop` boundaries and dropping the entire block.
- **B3 LinearMul generalization**: the current `try_linear_loop` (`optimize.rs:136-168`) requires net pointer delta 0 and head cell `-1`. Using A2 to relax to:
  - Head cell `-k` with `gcd(k, 256) == 1` (still guarantees termination)
  - Nested `LinearMul` bodies recognizable by the outer level (currently a body containing `Loop` aborts immediately)
  - A `LinearMul` fused-copy form (columns whose factor is only ±1)
- **B4 Pointer postponement (operation offsets / offset-form)** · **[landed]**: standard industry naming ([Nayuki](https://www.nayuki.io/page/optimizing-brainfuck-compiler), [matslina](https://github.com/matslina/bfoptimization) call it "operation offsets", bfc calls it "postponing movements"). Ultimately realized as a LIR-only pass (no new HIR variants, avoiding pollution of the HIR interpreter and existing pattern detectors): `src/ir/lir_postpone.rs`'s `postpone_pointer_adds` accumulates `virt_ptr: isize` plus `pending: BTreeMap<isize, PendingOp>` over a straight-line window. On encountering a barrier (`Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte`, or a `CellAddAt` / `CellSetAt` left over from a prior pass), or when `virt_ptr` would cross the disp8 boundary, a flush is triggered; the flush first emits probe `PtrAdd`s visiting the `(lo, hi)` extremes, delegating to the backend's `ensure_tape_contains_r15` for tape doubling and relying on the contiguity of the tape mapping to guarantee that every offset in the window is already bounds-checked. disp is capped at `[-127, 127]` (disp32 deferred until C2 lands). Runs before `optimize_lir(lower_to_lir(hir))` at `-O1` and above.
  - Files: `src/ir/lir_postpone.rs` (18 unit tests including the safety proof); integration point `src/driver/run.rs:build_optimized_lir`
- **B5 Loop-invariant code motion**: A3 can identify cells that are read but not written inside a loop; their pre-loop assignments can be hoisted outside the loop, avoiding repeated reloads. Utility is low on BF, but essentially free after SSA-isation.
- **B6 Small loop unrolling**: when the head cell value is known on loop entry (A3) and the body is affine, compute the `Add` / `Zero` sequence at compile time instead of producing a `LinearMul`.
- **B7 Deep balanced loop (K6 algorithm, optional)**: the K6 algorithm from [Oizys](https://github.com/jjcmoon/Oizys) performs a unified analysis of nested balanced loops, covering program classes that `try_linear_loop` + `try_scan_loop` cannot reach. A research extension once B3 / B4 are stable.

**Dependencies: B1 has landed (syntactic form); B2 / B3 directly depend on A; B4 is the prerequisite for C3; B5 / B6 can parallelize; B7 depends on B3 + B4 + A2.**

### Phase C — New LIR peephole layer

Goal: add `src/ir/lir_opt.rs` after `src/ir/lower.rs` to provide a no-analysis peephole pass.

- **C1 Base LIR peephole pass** · **[landed]**: merge adjacent `PtrAdd`, eliminate zero deltas, fold `CellSet(0); CellAdd(k)` → `CellSet(k)` and `CellSet(a); CellSet(b)` → `CellSet(b)`. Landed in `src/ir/lir_opt.rs`, chained after `lower_to_lir`; `Label` / `JumpIfZero` / `JumpIfNonZero` / `Scan` / `LinearMul` / `PutByte` / `GetByte` serve as natural barriers.
- **C2 Bounds-check hoisting / batching**: a run of `PtrAdd`s should bound-check only once at the extremes of the total delta (currently each `PtrAdd` checks independently, `codegen.rs:78-107`). This requires passing an "already-checked interval" marker to the backend; consider introducing `PtrAddChecked { delta, lo_extent, hi_extent }` or a new op on LIR.
- **C3 Displacement-form lowering** · **[landed]**: two new LIR variants `LirInst::CellAddAt { off, delta }` / `CellSetAt { off, val }` (`src/ir/lir.rs`) carry B4's flush output; the backend in `src/backend/codegen.rs` translates them to `AsmInst::AddMem8ImmDisp8` / `MovMem8ImmDisp8` (`src/backend/asm.rs`), encoded by `emit_mem8_disp8` in `src/backend/x86_64/encode.rs` as `add byte [r13 + disp8], imm8` / `mov byte [r13 + disp8], imm8` (ModRM mod=01; with R13 as base, `(rm & 7) == 5` avoids the SIB ambiguity, yielding machine code `49 80 45 <disp> <imm>` / `49 C6 45 <disp> <imm>`). `off == 0` is guarded by a `debug_assert!` in codegen — B4 canonicalises it back to `CellAdd` / `CellSet` (preserving D1's inc/dec short form); disp is enforced via `i8::try_from` to sit in `[-128, 127]`. disp32 is deferred until C2 lands — reason: offsets beyond disp8 must be paired with a range bounds-check or `ensure_tape_contains_r15`'s point-check semantics would be broken. The LIR peephole (`src/ir/lir_opt.rs`) gained 4 same-offset fold rules (`CellAddAt;CellAddAt`, `CellSetAt;CellAddAt`, `CellSetAt;CellSetAt`, `CellAddAt;CellSetAt`); cross-offset pairs never merge.
  - Files: `src/ir/lir.rs`, `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/codegen.rs`, `src/ir/lir_opt.rs`
- **C4 Propagate size hints to Scan / Zero**: let the backend know whether a tape segment before a particular `Scan` has already been bounds-checked, so it can decide whether to inline the bounds check.

**Dependencies: C1 is independent; C2 depends on A2's range; C3 depends on B4; C4 depends on C2.**

### Phase D — Backend / codegen (depends on the semantic guarantees of Phase C)

- **D1 Instruction selection** · **[landed]**: `CellAdd(±1)` → `inc` / `dec` (opcode `0xFE`); `add/and/cmp r, imm` immediates automatically pick between `0x83 + imm8` (4 bytes) and `0x81 + imm32` (7 bytes); long Jcc / JMP are iteratively shrunk to rel8 short jumps by a dedicated relaxation pass (`src/backend/x86_64/relax.rs`).
  - Files: `src/backend/asm.rs`, `src/backend/x86_64/encode.rs`, `src/backend/x86_64/relax.rs`, `src/backend/codegen.rs`
- **D2 SIMD-specialized forms**:
  - `Scan(±1)` → `rep scasb` (`al = 0`, `rdi = r13`, `rcx` set large enough) paired with a single bounds tightening. Needs to confirm the Windows ABI has no special requirements for `rep` instructions.
  - Contiguous `Zero` runs (from a future pass that merges them) → `rep stosb`
  - `LinearMul` columns with factor ±1 → `movzx + add`, paired with C3's displacement-form lowering
- **D3 Buffered I/O**: introduce a runtime-side I/O buffer (e.g. 4KB); `PutByte` writes to the buffer, flushed when full or on exit; `GetByte` reads from the buffer. Reduces syscall overhead by an order of magnitude. Requires adding `BufferedStdIo` to `src/runtime/io.rs` and pointing `PutByte` / `GetByte` at the new runtime entry points.
- **D4 Minimal register allocator**: track the explicit use of `rbx / rax / rcx` for `LinearMul` / loop-head multiplicands and source values (currently `codegen.rs:143-159` hard-codes them). Not a general RA — just a local "redundant-mov elimination" allocator. Paves the way for more aggressive codegen in Phase F.
- **D5 Jump alignment + branch hints**: align loop heads to 16B; attach `2e` / `3e` branch-hint prefixes to `JumpIfZero` (no-op on Intel, still parsed by AMD) — lowest priority.

**Dependencies: D1, D3 are independent; D2, D4 depend on C; D5 shares `encode.rs` with D1 and can parallelize.**

### Phase E — Interpreter + runtime (mostly independent from A–D, parallelizable)

- **E1 Super-instruction lowering**: introduce a bytecode representation between HIR and the interpreter (e.g. `Vec<InterpOp>` where `InterpOp` includes fused forms like `MoveAdd(d, k)` / `ZeroMove(d)`), eliminating the hot-path dispatch overhead of `engine.rs:66-132`.
- **E2 Threaded dispatch**: on top of E1, use an array of `fn(&mut State)` pointers in place of `match`, approximating computed-goto (stable Rust has no computed-goto). Each `InterpOp` gets a tail-call-style handler.
- **E3 SIMD tape operations**: `Zero` → `memset`; columns of `LinearMul` with factor 1 → `copy_from_slice`. `Tape::move_ptr`'s zero-fill already lowers to `memset` via `Vec::resize`, but `LinearMul`'s inner loop remains scalar (`engine.rs:103-111`).
- **E4 Tape backend restructuring** · **[landed, revised design]**: the original plan called for mmap + centered-copy, but that conflicts with `#![forbid(unsafe_code)]`. Revised: keep the current `Vec<u8>` left/right-spliced layout but switch to geometric doubling (`new_len = max(needed, old_len * 2)`, with the left half using an additional 8-byte floor due to its initial empty state): amortized O(1) per accessed cell, avoiding the O(n) resize that a single boundary-crossing step would otherwise trigger. `TapeStats::right_growth` was renamed to `right_grew_bytes` to clarify its semantics. Should a shared backend-tape call for a real mmap version later, it can be introduced under a runtime feature flag without breaking `forbid(unsafe)`.
- **E5 Criterion micro-benchmark suite** · **[landed]**: added `benches/`, using a subset of the [matslina standard benchmark suite](https://github.com/matslina/bfoptimization) — **factor.b**, **mandelbrot.b**, **hanoi.b**, **dbfi.b**, **long.b**, and **awib-0.4.b**. Measures O0 / O1 / O2 / O3 × (interpret, compile+run). These programs cover different contraction ratios (40%–75%) and different hot-loop patterns; they are the established BF-optimization-literature benchmarks. Reference speedup ranges from the literature: hanoi.b ≈ 130×, mandelbrot.b several tens of times, awib-0.4 ≈ 2.4× (full opt vs no opt). Serves as the regression baseline for all A–D passes.

> **E5 should land first**: the payoff of every subsequent optimization must be quantified through it.

**Dependencies: E5 has none, first to land; E1 → E2; E3 has none; E4 and D3's I/O rework can share a milestone.**

### Phase F — Long-term goals (outside the near-term dependency graph)

- **F1 Tiered JIT**: the interpreter collects loop trip counts, and hot regions switch to backend-generated machine code. Two implementation paths:
  - Hand-rolled (reusing the existing `src/backend/x86_64/encode.rs`): self-contained, but `mmap(PROT_EXEC)` necessarily breaks `#![forbid(unsafe_code)]` — the exemption scope needs to be explicitly agreed on.
  - [Cranelift](https://cranelift.dev/) as the JIT backend: a mature codegen framework, used by several BF-JIT case studies (e.g. [Rodrigodd's Part 3](https://rodrigodd.github.io/2022/11/26/bf_compiler-part3.html)), but breaks the "zero runtime dependencies" promise; the tradeoff mirrors F4 LLVM.
- **F2 ARM64 backend**: Linux aarch64 + macOS arm64. Register conventions need to be redesigned (`x12/x13/x14/x15` remapping), and the encode layer rewritten from scratch.
- **F3 macOS Mach-O x86_64 backend**: smaller workload than ELF, but the syscall numbers change, and `LC_SEGMENT_64` / `LC_MAIN` file-format code must be added.
- **F4 LLVM backend (optional)**: the cost is breaking the "zero runtime dependencies" promise; exists behind a toggleable feature flag.
- **F5 Incremental compilation cache**: cache HIR / LIR / obj for a fixed `.bf` source keyed by content hash. Only worth it if E5 shows compile time is a significant fraction of total time.

---

## 3. Dependency Overview

```
E5 (bench)  ──────────────────────────────────┐
                                              ▼
A1 → A2 → A3 → A4                        (regression baseline)
  │    │    │    │
  │    │    │    └→ B1 (DSE) ✓
  │    │    └→ B2 (zero-loop), B5 (LICM), B6 (unroll)
  │    └→ B3 (LinearMul generalization), B4 (pointer postponement) ✓, B7 (K6)
  │         │
  │         └→ C3 (displacement) ✓ → D2 / D4
  │
  └→ C2 (bounds batching) → D2 (SIMD)

C1 (LIR peephole), D1 (instruction selection), D3 (buffered I/O),
E1 / E2 (super-instructions + threaded dispatch),
E3 (SIMD tape), E4 (tape doubling) can all start in parallel.
Landed: E5, C1, D1, E4, Phase A (A1–A4), B1, B4, C3.

Phase F items are all outside the near-term dependency graph.
```

---

## 4. Files of Interest

| Layer | File | Phase |
|---|---|---|
| HIR | `src/ir/hir.rs` | B3 / B4 if new variants needed |
| HIR | `src/ir/optimize.rs` | Main edit point for Phases A / B |
| HIR | `src/ir/analysis/` | A1–A4 skeletons (landed) |
| HIR | `src/ir/dse.rs` | B1 DSE (landed) |
| LIR | `src/ir/lir.rs`, `src/ir/lower.rs` | B / C may add `PtrAddChecked` / `CellAddAt` |
| LIR | `src/ir/lir_opt.rs` (new) | Main venue for Phase C |
| Backend | `src/backend/codegen.rs`, `src/backend/x86_64/encode.rs`, `src/backend/asm.rs` | Phase D |
| Backend | `src/backend/x86_64/elf.rs`, `src/backend/x86_64/windows.rs` | D3 buffered I/O |
| Runtime | `src/interp/engine.rs`, `src/runtime/{tape,io,host}.rs` | Phase E |
| Bench | `benches/` (new) | E5 |
| Tests | `tests/cases_pipeline.rs`, `tests/compile_artifacts.rs` | Regression after each phase |

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
- Do not break `#![forbid(unsafe_code)]`; Phase F1 JIT's exemption scope is discussed separately and is not part of the near-term roadmap.
- Both language editions (`docs/OPTIMIZATION_PLAN.md` and `docs/OPTIMIZATION_PLAN_CN.md`) must be updated in lockstep.

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
