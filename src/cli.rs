use crate::driver::config::{DriverConfig, RunMode};

/// print IR:
/// cargo run -- dump "+++++>---[->+<]."
///
/// interpreter:
/// cargo run -- run "++++++++[>++++++++<-]>+."
pub fn parse_cli() -> Result<DriverConfig, String> {
    let mut args = std::env::args();
    let _exe = args.next();

    let usage =
        "usage:\n  cargo run -- dump \"BF_CODE\"\n  cargo run -- run  \"BF_CODE\"".to_string();

    let mode_str = args.next().ok_or_else(|| usage.clone())?;

    let source = args.next().ok_or_else(|| usage.clone())?;

    let mode = match mode_str.as_str() {
        "dump" => RunMode::DumpIr,
        "run" => RunMode::Interpret,
        other => {
            return Err(format!("unknown mode: {}\n{}", other, usage.clone()));
        }
    };

    Ok(DriverConfig { source, mode })
}
