# Ralph Fix Plan — AmazingBF Optimization Roadmap

Tracks progress on the OPTIMIZATION_PLAN.md roadmap items.

## Completed
- [x] H1 — Whole-program JIT (`-m jit`)
- [x] G1 — Extract shared codegen logic into `codegen_common.rs`
- [x] H2 — Ret-based JIT execution (`compile_lir_to_jit_asm`, `JitTape`, `execute_fn`)
- [x] E5/H — JIT benchmark integration: `jit/O<N>` groups in `standard_suite.rs` (Criterion) + JIT timing rows in `compile_levels.rs` (custom harness)
- [x] F1b-P0 — Loop trip-count profiling infrastructure (`LoopProfile`, wired into interpreter `LoopEnd` handler)

## Next Up (by priority)
- [ ] F1b-P1 — Tiered JIT: extract hot loop bytecode range → HIR → LIR → JIT, replace dispatch with JIT call
- [ ] F1b-P2 — CLI `--profile-loops` flag for profiling output
- [ ] E5 follow-ups — Benchmark CI integration, baseline tracking

## Notes
- The full roadmap lives in `docs/OPTIMIZATION_PLAN.md`
- Main crate uses `#![forbid(unsafe_code)]`; all unsafe lives in `crates/jit/`
- H2 unblocks F1b (tiered JIT) and JIT benchmark integration with E5
- Update this file after each major milestone
