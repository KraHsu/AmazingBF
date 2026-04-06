use anyhow::{Result, bail};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub(crate) fn init_logger(log_level: u8) -> Result<()> {
    let targets = log_targets(log_level)?;
    let include_source_location = cfg!(debug_assertions);

    let want_json = json_logs_enabled() && cfg!(feature = "json-logs");

    if want_json {
        #[cfg(feature = "json-logs")]
        tracing_subscriber::registry()
            .with(targets.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_file(include_source_location)
                    .with_line_number(include_source_location)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(targets)
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_file(include_source_location)
                    .with_line_number(include_source_location),
            )
            .init();
    }

    Ok(())
}

fn log_targets(log_level: u8) -> Result<Targets> {
    if let Some(t) = rust_log_targets_simple() {
        return Ok(t);
    }
    default_targets(log_level)
}

fn default_targets(log_level: u8) -> Result<Targets> {
    let crate_name = env!("CARGO_CRATE_NAME");
    Ok(match log_level {
        0 => Targets::new().with_default(LevelFilter::OFF),
        1 => Targets::new()
            .with_default(LevelFilter::WARN)
            .with_target(crate_name, LevelFilter::INFO),
        2 => Targets::new()
            .with_default(LevelFilter::WARN)
            .with_target(crate_name, LevelFilter::DEBUG),
        3 => Targets::new()
            .with_default(LevelFilter::INFO)
            .with_target(crate_name, LevelFilter::DEBUG),
        4 => Targets::new().with_default(LevelFilter::DEBUG),
        _ => bail!("log level must be between 0 and 4, got {log_level}"),
    })
}

/// Single-token `RUST_LOG` (e.g. `debug`) applies as a global level. Module/path filters with `=` or `,` are ignored here (use a debug build or extend this parser if needed).
fn rust_log_targets_simple() -> Option<Targets> {
    let s = std::env::var("RUST_LOG").ok()?;
    let s = s.trim();
    if s.is_empty() || s.contains('=') || s.contains(',') {
        return None;
    }
    let lvl = match s.to_ascii_lowercase().as_str() {
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        "off" => LevelFilter::OFF,
        _ => return None,
    };
    Some(Targets::new().with_default(lvl))
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
