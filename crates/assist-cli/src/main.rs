//! `assist`: the command-line entry point (PLAN.md §7.1).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod repl;

/// Offline-first personal automation and voice assistant.
#[derive(Parser)]
#[command(name = "assist", version, about)]
struct Cli {
    /// Config file [default: config.toml in the user config directory].
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Data directory for the audit log [default: the user data directory].
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive shell: type commands, `$ cmd` for the system shell, `help` for more.
    Shell,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = repl::Paths::resolve(cli.config, cli.data_dir)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    match cli.command {
        Command::Shell => runtime.block_on(repl::run(paths)),
    }
}
