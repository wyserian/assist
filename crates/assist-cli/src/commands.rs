//! One-shot commands that talk to the running daemon.

use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{bail, Result};
use assist_proto::rpc::{self, Explain, Explanation, JobRef, SecretEntry, SecretName, SecretSet};
use assist_proto::{JobState, JobSummary, JobUpdate, Stream};

use crate::client::Client;
use crate::display::{ended_line, job_row, output_line, printable};
use crate::setup::Paths;

/// Says `text`, answers any questions on the terminal, and follows the started jobs to
/// the end. Fails if a job did not finish successfully.
pub async fn run(paths: &Paths, text: &str) -> Result<ExitCode> {
    let mut client = Client::connect(paths.data()).await?;
    let mut reply = client.say(text).await?;
    let mut running = HashSet::new();
    loop {
        for line in &reply.say {
            println!("{}", printable(line));
        }
        for job in &reply.jobs {
            println!("[#{}] {}", job.id, printable(&job.title));
            running.insert(job.id);
        }
        if !reply.waiting {
            break;
        }
        if !io::stdin().is_terminal() {
            bail!("the assistant asked a question; run this in a terminal to answer it");
        }
        let answer = read_line("> ").await?;
        reply = client
            .say(if answer.is_empty() { "cancel" } else { &answer })
            .await?;
    }

    let mut failed = false;
    while !running.is_empty() {
        match client.update().await? {
            JobUpdate::Output { id, stream, text } if running.contains(&id) => {
                println!("{}", output_line(id, stream == Stream::Err, &text));
            }
            JobUpdate::State { id, state, error } if running.contains(&id) => {
                if let Some(line) = ended_line(id, state, error.as_deref()) {
                    println!("{line}");
                    running.remove(&id);
                    failed |= state != JobState::Done;
                }
            }
            _ => {}
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Lists active and recent jobs.
pub async fn jobs(paths: &Paths) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    let jobs: Vec<JobSummary> = client.call(rpc::JOBS_LIST, ()).await?;
    if jobs.is_empty() {
        println!("No jobs yet.");
    }
    for job in &jobs {
        println!("{}", job_row(job));
    }
    Ok(())
}

/// Stops one job (`<id>`) or every job (`all`).
pub async fn stop(paths: &Paths, target: &str) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    if target == "all" {
        let stopped: usize = client.call(rpc::JOBS_STOP_ALL, ()).await?;
        println!("Stopping {stopped} job(s).");
        return Ok(());
    }
    let Ok(id) = target.trim_start_matches('#').parse::<u64>() else {
        bail!("give a job id or all");
    };
    let stopping: bool = client.call(rpc::JOBS_STOP, JobRef { id }).await?;
    if stopping {
        println!("Stopping #{id}.");
    } else {
        println!("No running job #{id}.");
    }
    Ok(())
}

/// Prints a job's last output.
pub async fn log(paths: &Paths, id: u64) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    let lines: Vec<String> = client.call(rpc::JOBS_TAIL, JobRef { id }).await?;
    for line in &lines {
        println!("{}", printable(line));
    }
    Ok(())
}

/// Shows what `text` would do, without doing it.
pub async fn explain(paths: &Paths, text: &str) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    let explanation: Option<Explanation> = client
        .call(
            rpc::COMMANDS_EXPLAIN,
            Explain {
                text: text.to_owned(),
            },
        )
        .await?;
    let Some(explanation) = explanation else {
        println!("No command matches that.");
        return Ok(());
    };
    println!("command:  {}", explanation.command);
    for (slot, value) in &explanation.slots {
        println!("  {slot} = {}", printable(value));
    }
    for (slot, options) in &explanation.ambiguous {
        println!("  {slot} could be: {}", printable(&options.join(", ")));
    }
    println!("action:   {}", printable(&explanation.readback));
    println!("decision: {}", explanation.decision);
    Ok(())
}

/// Lists secret names; values are never shown.
pub async fn secret_list(paths: &Paths) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    let secrets: Vec<SecretEntry> = client.call(rpc::SECRETS_LIST, ()).await?;
    if secrets.is_empty() {
        println!("No secrets.");
    }
    for secret in &secrets {
        println!("{:<24} ...{}", secret.name, printable(&secret.last4));
    }
    Ok(())
}

/// Stores a secret read from stdin (one line).
pub async fn secret_set(paths: &Paths, name: &str) -> Result<()> {
    let prompt = if io::stdin().is_terminal() {
        "Value (shown while typing; pipe it in or use the dashboard to keep it hidden): "
    } else {
        ""
    };
    let value = read_line(prompt).await?;
    if value.is_empty() {
        bail!("no value given");
    }
    let mut client = Client::connect(paths.data()).await?;
    let secret = SecretSet {
        name: name.to_owned(),
        value,
    };
    let _: bool = client.call(rpc::SECRETS_SET, secret).await?;
    println!("Stored {name}.");
    Ok(())
}

/// Deletes a secret.
pub async fn secret_delete(paths: &Paths, name: &str) -> Result<()> {
    let mut client = Client::connect(paths.data()).await?;
    let name = SecretName {
        name: name.to_owned(),
    };
    let deleted: bool = client.call(rpc::SECRETS_DELETE, &name).await?;
    if deleted {
        println!("Deleted {}.", name.name);
    } else {
        println!("No secret {}.", name.name);
    }
    Ok(())
}

/// Reads one line from stdin without blocking the runtime; empty at end of input.
async fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).map(|_| line)
    })
    .await??;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}
