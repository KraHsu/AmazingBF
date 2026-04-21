# Contributing

## Development Baseline

- Use Rust `1.94` or newer (same as `rust-version` in `Cargo.toml`).
- Run `cargo fmt` before sending changes.
- Run `cargo clippy --all-targets -- -D warnings` for lint checks.
- Run `cargo test` for the regression suite.
- Run `cargo bench --bench compile_levels` for the compile-then-run timing table over `tests/cases/*.bf` at `-O0..3`.
- Run `cargo bench --bench standard_suite` for per-level interp / exec timings on the matslina BF suite (supports Criterion `--save-baseline` / `--baseline`).

## Architecture Rules

- Keep the core pipeline consistent: source -> lexer -> parser -> AST -> HIR -> optimize -> LIR.
- `interpret` mode stops at optimized HIR and executes in `src/interp/engine.rs`.
- `dump` and `compile` are the only modes that continue into LIR / backend assembly.
- `compile` mode must keep Linux ELF and Windows PE64 output behavior aligned with `README.md` and `man/amazingbf.1`.

## Code Style

- Prefer `pub(crate)` for implementation details; expand visibility only when a stable public API is intentional.
- Use module docs to explain responsibilities and invariants, especially around IR, backend, and runtime code.
- Keep function comments factual and compact. Explain why or what invariant matters; avoid narrating obvious assignments.
- Preserve existing user-facing CLI behavior unless the change explicitly targets CLI UX.

## Tests

- `tests/cases_pipeline.rs` covers end-to-end interpreter and compiler output against fixtures.
- `tests/windows_target.rs` covers PE64 layout and cross-target behavior.
- `tests/compile_artifacts.rs` validates ELF/PE artifacts, `.asm`/`.lst` output, and EOF semantics across O0–O3.
- `tests/cases/*.bf` are fixtures, not Rust source files; keep fixture naming aligned across `.bf`, `.in`, `.out`, and `.md`.

## Benchmarks

- `benches/compile_levels.rs` owns compile + run timing tables over `tests/cases/*.bf` (custom harness; no Criterion).
- `benches/standard_suite.rs` owns interp/exec timings on the matslina BF programs (Criterion).
- Both are developer-local; CI runs `cargo test` only.

## Documentation

- Update `README.md` when architecture, CLI behavior, outputs, or workflow changes, and keep `docs/README_CN.md` in sync.
- Update `man/amazingbf.1` when CLI help or environment variables change.
- Update `.cursor/rules/project-architecture.mdc` when pipeline or backend facts change.
- When contributing guidelines change, update both `CONTRIBUTING.md` and `docs/CONTRIBUTING_CN.md`.
