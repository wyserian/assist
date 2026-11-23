//! `assist shell`: the core running in-process behind a line-based prompt (Phase 1a).
//!
//! Lines go to the dialog; confirmed plans become jobs whose output is printed live as
//! `[#id] …`. A few words are handled here instead: `jobs`, `log <id>`, `stop <id>`,
//! `stop all`, `help`, `exit`. When input ends (piped stdin), the shell waits for running
//! jobs; `exit` and Ctrl+C stop them.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use assist_core::action::Chunk;
use assist_core::audit::Audit;
use assist_core::config::Config;
use assist_core::dialog::{Dialog, Engine};
use assist_core::phrases::PhraseSet;
use assist_core::policy::Principal;
use assist_core::scheduler::{JobEvent, JobId, JobState, Scheduler};
use directories::ProjectDirs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast::{self, error::RecvError};

const HELP: &str = "\
Say what you want, e.g. `open firefox`, `run build`, `search rust tokio`, `text mom`.
  $ <command>   run in the system shell (`$ cd <dir>` changes directory)
  jobs          list jobs
  log <id>      show a job's last output
  stop <id>     stop a job        stop all   stop every job
  cancel        drop the current question
  exit          stop all jobs and quit";

/// Where config and data live.
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
}

impl Paths {
    /// Explicit paths, or the user's config and data directories.
    pub fn resolve(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "assist");
        let config = match config {
            Some(path) => path,
            None => dirs
                .as_ref()
                .map(|d| d.config_dir().join("config.toml"))
                .context("no user config directory; pass --config")?,
        };
        let data = match data {
            Some(path) => path,
            None => dirs
                .as_ref()
                .map(|d| d.data_dir().to_owned())
                .context("no user data directory; pass --data-dir")?,
        };
        Ok(Self { config, data })
    }
}

struct Repl {
    scheduler: Scheduler,
    engine: Arc<Engine>,
    dialog: Dialog,
    owner: Principal,
    cwd: PathBuf,
}

enum Flow {
    Continue,
    Exit,
}

/// Runs the shell until input ends, `exit` or Ctrl+C.
pub async fn run(paths: Paths) -> Result<()> {
    let config = if paths.config.exists() {
        Config::load(&paths.config).with_context(|| format!("config {}", paths.config.display()))?
    } else {
        println!(
            "No config at {}; starting empty (see config/config.example.toml).",
            paths.config.display()
        );
        Config::from_toml("")?
    };
    let config = Arc::new(config);
    let audit_dir = paths.data.join("audit");
    let audit =
        Audit::open(&audit_dir).with_context(|| format!("audit log {}", audit_dir.display()))?;
    let scheduler = Scheduler::new(
        assist_actions::builtin(),
        Arc::clone(&config),
        Some(audit.clone()),
    );
    let phrases = PhraseSet::default_en()?;
    let engine = Arc::new(Engine::new(
        Arc::clone(&config),
        &phrases,
        scheduler.commands(),
        true,
    )?);
    let owner = Principal::local("cli");
    let mut repl = Repl {
        dialog: Dialog::new(Arc::clone(&engine), owner.clone()),
        scheduler,
        engine,
        owner,
        cwd: std::env::current_dir()?,
    };
    let mut events = repl.scheduler.subscribe();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    println!(
        "assist {} — type `help` for commands.",
        env!("CARGO_PKG_VERSION")
    );
    prompt();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if let Flow::Exit = repl.handle(&line) {
                    repl.scheduler.cancel_all();
                    break;
                }
                prompt();
            }
            event = events.recv() => match event {
                Ok(event) => repl.show(event),
                Err(RecvError::Lagged(skipped)) => println!("({skipped} job events skipped)"),
                Err(RecvError::Closed) => break,
            },
            _ = tokio::signal::ctrl_c() => {
                let stopped = repl.scheduler.cancel_all();
                println!("\nStopping {stopped} job(s).");
                break;
            }
        }
    }
    repl.wait_for_jobs(&mut events).await;
    if let Some(err) = audit.last_error() {
        eprintln!("audit log error: {err}");
    }
    Ok(())
}

fn prompt() {
    print!("> ");
    let _ = std::io::stdout().flush();
}

impl Repl {
    fn handle(&mut self, line: &str) -> Flow {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["exit" | "quit"] => return Flow::Exit,
            ["help"] => println!("{HELP}"),
            ["jobs"] => self.list_jobs(),
            ["log", id] => match parse_id(id) {
                Some(id) => self.log(id),
                None => println!("Usage: log <id>"),
            },
            ["stop", "all"] => println!("Stopping {} job(s).", self.scheduler.cancel_all()),
            ["stop", id] if parse_id(id).is_some() => {
                let id = parse_id(id).unwrap_or_default();
                if self.scheduler.cancel(id) {
                    println!("Stopping #{id}.");
                } else {
                    println!("No running job #{id}.");
                }
            }
            _ => self.converse(line),
        }
        Flow::Continue
    }

    fn converse(&mut self, line: &str) {
        let reply = self.dialog.handle(&self.owner, line, Instant::now());
        for say in reply.say {
            println!("{say}");
        }
        for plan in reply.run {
            let title = self.engine.readback(&plan);
            match self
                .scheduler
                .submit(plan, self.owner.clone(), title.clone(), self.cwd.clone())
            {
                Ok(id) => println!("[#{id}] {title}"),
                Err(err) => println!("{err}"),
            }
        }
    }

    fn show(&mut self, event: JobEvent) {
        match event {
            JobEvent::Output { id, chunk } => match chunk {
                Chunk::Line(text) | Chunk::Info(text) => println!("[#{id}] {text}"),
                Chunk::Error(text) => println!("[#{id}] ! {text}"),
                Chunk::Cwd(path) => self.cwd = path,
            },
            JobEvent::State { id, state } => match state {
                JobState::Queued | JobState::Running => {}
                JobState::Failed => {
                    let reason = self
                        .scheduler
                        .get(id)
                        .and_then(|job| job.error)
                        .map_or_else(String::new, |err| format!(": {err}"));
                    println!("[#{id}] failed{reason}");
                }
                JobState::Cancelled => println!("[#{id}] stopped"),
                JobState::Done | JobState::TimedOut => println!("[#{id}] {}", state.as_str()),
            },
        }
    }

    fn list_jobs(&self) {
        let jobs = self.scheduler.list();
        if jobs.is_empty() {
            println!("No jobs yet.");
        }
        for job in jobs {
            let lane = format!("{:?}", job.lane).to_lowercase();
            println!(
                "#{:<4} {:<10} {:<12} {}",
                job.id,
                job.state.as_str(),
                lane,
                job.title
            );
        }
    }

    fn log(&self, id: JobId) {
        let Some(job) = self.scheduler.get(id) else {
            return println!("No job #{id}.");
        };
        println!("#{id} {} ({})", job.title, job.state.as_str());
        for line in &job.tail {
            println!("  {line}");
        }
        if let Some(err) = job.error {
            println!("  error: {err}");
        }
    }

    /// Prints events until every job has ended.
    async fn wait_for_jobs(&mut self, events: &mut broadcast::Receiver<JobEvent>) {
        while self
            .scheduler
            .list()
            .iter()
            .any(|job| !job.state.is_finished())
        {
            match events.recv().await {
                Ok(event) => self.show(event),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
        // Events sent just before the last job finished.
        while let Ok(event) = events.try_recv() {
            self.show(event);
        }
    }
}

fn parse_id(text: &str) -> Option<JobId> {
    text.trim_start_matches('#').parse().ok()
}
