//! `bf-interpreter` binary: interpret-only Brainfuck front-end.
//!
//! Delegates to [`AmazingBF::run_bf_interpreter`], which locks the CLI to
//! `RunMode::Interpret` (no `-m`, no `--target`).

fn main() -> AmazingBF::Result<()> {
    AmazingBF::run_bf_interpreter()
}
