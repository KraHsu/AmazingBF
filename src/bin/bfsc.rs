//! `bfsc` binary: BFS (Brainf Script) → Brainfuck source compiler.
//!
//! Surface for [`AmazingBF::run_bfsc`]; errors are printed with a `bfsc:`
//! prefix and the process exits with status `1` on failure.

fn main() {
    if let Err(e) = AmazingBF::run_bfsc() {
        eprintln!("bfsc: {e}");
        std::process::exit(1);
    }
}
