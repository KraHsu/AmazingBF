# Contributing

## Development Baseline

- Use Rust `1.85` or newer.
- Run `cargo fmt` before sending changes.
- Run `cargo clippy --all-targets -- -D warnings` for lint checks.
- Run `cargo test` for the fast regression suite.
- Run `cargo test --test compile_pipeline -- --ignored --nocapture` only when you need the slow compile-mode benchmark-style regression.

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
- `tests/compile_pipeline.rs` is intentionally slow and ignored by default.
- `tests/cases/*.bf` are fixtures, not Rust source files; keep fixture naming aligned across `.bf`, `.in`, `.out`, and `.md`.

## Documentation

- Update `README.md` when architecture, CLI behavior, outputs, or workflow changes.
- Update `man/amazingbf.1` when CLI help or environment variables change.
- Update `.cursor/rules/project-architecture.mdc` when pipeline or backend facts change.
