//! `assist`: the command-line entry point (PLAN.md section 7.1).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod commands;
mod daemon;
mod display;
mod keystore;
#[cfg(feature = "voice")]
mod listen;
#[cfg(feature = "voice")]
mod models;
mod repl;
mod setup;

/// Offline-first personal automation and voice assistant.
#[derive(Parser)]
#[command(name = "assist", version, about)]
struct Cli {
    /// Config file [default: config.toml in the user config directory].
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Data directory for the audit log, job history and token [default: the user data
    /// directory].
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon on the loopback address from [server] bind.
    Serve,
    /// Say something to the daemon and follow the jobs it starts, e.g. assist run open firefox.
    Run {
        /// What to say.
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// List active and recent jobs.
    Jobs,
    /// Stop a job by id, or every job with "all".
    Stop {
        /// Job id or "all".
        target: String,
    },
    /// Show the last output of a job.
    Log {
        /// Job id.
        id: u64,
    },
    /// Show what a sentence would do, without doing it.
    Explain {
        /// The sentence.
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// Open the dashboard in the default browser, signed in.
    Ui {
        /// Print the one-time sign-in link instead of opening a browser.
        #[arg(long)]
        print: bool,
    },
    /// Manage secrets (API keys). Values are never shown.
    #[command(subcommand)]
    Secret(SecretCommand),
    /// Interactive shell without a daemon: type commands, "$ cmd" for the system shell,
    /// "help" for more.
    Shell,
    /// Speak commands to the daemon and hear the replies. Asks before downloading the voice
    /// models the first time.
    #[cfg(feature = "voice")]
    Listen {
        /// Microphone whose name contains this text [default: the system default].
        #[arg(long)]
        device: Option<String>,
    },
}

#[derive(Subcommand)]
enum SecretCommand {
    /// List secret names.
    List,
    /// Store or replace a secret; the value is read from stdin.
    Set {
        /// Name: a-z, 0-9, _ and -.
        name: String,
    },
    /// Delete a secret.
    Delete {
        /// Name.
        name: String,
    },
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let paths = setup::Paths::resolve(cli.config, cli.data_dir)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match cli.command {
            Command::Serve => daemon::run(paths).await?,
            Command::Run { text } => return commands::run(&paths, &text.join(" ")).await,
            Command::Jobs => commands::jobs(&paths).await?,
            Command::Stop { target } => commands::stop(&paths, &target).await?,
            Command::Log { id } => commands::log(&paths, id).await?,
            Command::Explain { text } => commands::explain(&paths, &text.join(" ")).await?,
            Command::Ui { print } => commands::ui(&paths, print).await?,
            Command::Secret(SecretCommand::List) => commands::secret_list(&paths).await?,
            Command::Secret(SecretCommand::Set { name }) => {
                commands::secret_set(&paths, &name).await?;
            }
            Command::Secret(SecretCommand::Delete { name }) => {
                commands::secret_delete(&paths, &name).await?;
            }
            Command::Shell => repl::run(paths).await?,
            #[cfg(feature = "voice")]
            Command::Listen { device } => listen::run(&paths, device).await?,
        }
        Ok(ExitCode::SUCCESS)
    })
}
