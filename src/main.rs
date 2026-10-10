#[cfg(not(target_os = "macos"))]
compile_error!("pasted only supports macOS");

mod app;
mod cli;
mod clipboard;
mod hotkeys;
mod import;
mod link_preview;
mod login_item;
mod model;
mod ocr;
mod paste;
mod paths;
mod rich;
mod rules;
mod search;
mod settings;
mod shortcut;
mod snippet;
mod store;
mod transform;
mod ui;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PASTED_LOG").unwrap_or_else(|_| "pasted=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match cli::Cli::parse().command {
        None | Some(cli::Command::Run) => app::run(),
        Some(command) => cli::run(command),
    }
}
