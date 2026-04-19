//! Binary entry point for the primary `AmazingBF` command.
//!
//! Delegates straight to [`AmazingBF::run_amazingbf`]; all parsing, logging,
//! and execution live inside the library crate.

fn main() -> AmazingBF::Result<()> {
    AmazingBF::run_amazingbf()
}
