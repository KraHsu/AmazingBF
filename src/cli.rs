use crate::driver::config::DriverConfig;

/// cargo run -- "++>---[->+<]."
pub fn parse_cli() -> Result<DriverConfig, String> {
    let mut args = std::env::args();
    let _exe = args.next();

    let source = args
        .next()
        .ok_or_else(|| "usage: cargo run -- \"BF_CODE\"".to_string())?;

    Ok(DriverConfig { source })
}
