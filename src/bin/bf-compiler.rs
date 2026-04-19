//! `bf-compiler` binary: compile-only Brainfuck front-end.
//!
//! Delegates to [`AmazingBF::run_bf_compiler`], which locks the CLI to
//! `RunMode::Compile` and exposes `--target` for x86_64-linux / x86_64-windows.

fn main() -> AmazingBF::Result<()> {
    AmazingBF::run_bf_compiler()
}
