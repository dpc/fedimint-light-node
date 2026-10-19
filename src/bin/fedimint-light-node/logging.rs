//! The executable alone owns logging configuration; stdout remains command output.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// Install once before operational work, with a safe fallback for invalid filters.
pub fn init() {
    let builder = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(LevelFilter::INFO.into());
    // Never echo a malformed environment value: it may contain secrets.
    let (filter, invalid) = match builder.from_env() {
        Ok(filter) => (filter, false),
        Err(_) => (builder.parse("info").expect("valid default filter"), true),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    if invalid {
        tracing::warn!("invalid RUST_LOG; using info");
    }
}
