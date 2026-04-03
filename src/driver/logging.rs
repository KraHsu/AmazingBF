use anyhow::{Result, bail};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init_logger(log_level: u8) -> Result<()> {
    let default_filter = default_env_filter(log_level)?;
    let env_filter = EnvFilter::try_from_default_env().unwrap_or(default_filter);
    let include_source_location = cfg!(debug_assertions);

    if json_logs_enabled() {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_file(include_source_location)
                    .with_line_number(include_source_location)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_file(include_source_location)
                    .with_line_number(include_source_location),
            )
            .init();
    }

    Ok(())
}

fn default_env_filter(log_level: u8) -> Result<EnvFilter> {
    let crate_name = env!("CARGO_CRATE_NAME");

    match log_level {
        0 => Ok(EnvFilter::new("off")),
        1 => Ok(EnvFilter::new(format!("warn,{crate_name}=info"))),
        2 => Ok(EnvFilter::new(format!("warn,{crate_name}=debug"))),
        3 => Ok(EnvFilter::new(format!("info,{crate_name}=debug"))),
        4 => Ok(EnvFilter::new("debug")),
        _ => bail!("log level must be between 0 and 4, got {log_level}"),
    }
}

fn json_logs_enabled() -> bool {
    match std::env::var("AMAZINGBF_LOG_FORMAT") {
        Ok(value) if value.eq_ignore_ascii_case("json") => true,
        Ok(value) if value.eq_ignore_ascii_case("text") => false,
        Ok(_) => false,
        Err(_) => env_flag("AMAZINGBF_LOG_JSON"),
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}
