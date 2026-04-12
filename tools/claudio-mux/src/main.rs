mod cli;
mod config;
mod conpty;
mod host;
mod pane_state;
mod session;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let config = config::load_config()?;

    if config.general.require_windows_terminal {
        if std::env::var("WT_SESSION").is_err() {
            anyhow::bail!(
                "claudio-mux requires Windows Terminal (WT_SESSION not found).\n\
                 Set general.require_windows_terminal = false in config to override."
            );
        }
    }

    let log_dir = config::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "claudio-mux.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_env("CLAUDIO_MUX_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();

    tracing::info!("claudio-mux starting, session={}", cli.session);

    println!("claudio-mux v0.1 \u{2014} session: {}", cli.session);
    println!("Runtime not yet wired. Use --help for options.");

    Ok(())
}
