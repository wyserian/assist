//! `assist shell`: the core running in-process behind a line-based prompt.
//!
//! Lines go to the dialog; confirmed plans become jobs whose output is printed live as
//! `[#id] ...`. A few words are handled here instead: `jobs`, `log <id>`, `stop <id>`,
//! `stop all`, `help`, `exit`. When input ends (piped stdin), the shell waits for running
//! jobs; `exit` and Ctrl+C stop them.

use std::io::{self, Write};
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
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::broadcast::{self, error::RecvError};

const HELP: &str = "\
Say what you want, for example: open firefox, run build, search rust tokio.
  $ <command>   run in the system shell ($ cd <dir> changes directory)
  jobs          list jobs
  log <id>      show the last output of a job
  stop <id>     stop a job
  stop all      stop every job
  cancel        drop the current question
  exit          stop all jobs and quit";

/// Longer input lines are rejected instead of buffered.
const MAX_INPUT: usize = 8 * 1024;

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
    let mut input = LineReader::new(tokio::io::stdin());

    println!(
        "assist {}. Type help for commands.",
        env!("CARGO_PKG_VERSION")
    );
    prompt();
    loop {
        tokio::select! {
            line = input.next() => {
                match line? {
                    Input::End => break,
                    Input::TooLong => println!("Line too long (max {MAX_INPUT} bytes)."),
                    Input::Line(line) => {
                        if let Flow::Exit = repl.handle(&line) {
                            repl.scheduler.cancel_all();
                            break;
                        }
                    }
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
    let _ = io::stdout().flush();
}

/// `text` with control characters replaced, so job output cannot move the cursor, change the
/// terminal title or hide text.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
        .collect()
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
            println!("{}", printable(&say));
        }
        for plan in reply.run {
            let title = self.engine.readback(&plan);
            match self
                .scheduler
                .submit(plan, self.owner.clone(), title.clone(), self.cwd.clone())
            {
                Ok(id) => println!("[#{id}] {}", printable(&title)),
                Err(err) => println!("{err}"),
            }
        }
    }

    fn show(&mut self, event: JobEvent) {
        match event {
            JobEvent::Output { id, chunk } => match chunk {
                Chunk::Line(text) | Chunk::Info(text) => println!("[#{id}] {}", printable(&text)),
                Chunk::Error(text) => println!("[#{id}] ! {}", printable(&text)),
                Chunk::Cwd(path) => self.cwd = path,
            },
            JobEvent::State { id, state } => match state {
                JobState::Queued | JobState::Running => {}
                JobState::Failed => {
                    let reason = self
                        .scheduler
                        .get(id)
                        .and_then(|job| job.error)
                        .map_or_else(String::new, |err| format!(": {}", printable(&err.message)));
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
            println!(
                "#{:<4} {:<10} {:<12} {}",
                job.id,
                job.state.as_str(),
                job.lane.as_str(),
                printable(&job.title)
            );
        }
    }

    fn log(&self, id: JobId) {
        let (Some(job), Some(tail)) = (self.scheduler.get(id), self.scheduler.tail(id)) else {
            return println!("No job #{id}.");
        };
        println!("#{id} {} ({})", printable(&job.title), job.state.as_str());
        for line in &tail {
            println!("  {}", printable(line));
        }
        if let Some(err) = job.error {
            println!("  error: {}", printable(&err.message));
        }
    }

    /// Prints events until every job has ended.
    async fn wait_for_jobs(&mut self, events: &mut broadcast::Receiver<JobEvent>) {
        while self.scheduler.active() > 0 {
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

/// One line of user input.
enum Input {
    Line(String),
    TooLong,
    End,
}

/// Reads lines of at most [`MAX_INPUT`] bytes. Cancel-safe: a partial line is kept between
/// calls, so it can be used in `select!`.
struct LineReader<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
    overflow: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: Vec::new(),
            overflow: false,
        }
    }

    async fn next(&mut self) -> io::Result<Input> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.line.is_empty() && !self.overflow {
                    return Ok(Input::End);
                }
                return Ok(self.take());
            }
            let newline = available.iter().position(|&b| b == b'\n');
            let end = newline.map_or(available.len(), |at| at + 1);
            if self.line.len() + end > MAX_INPUT {
                self.overflow = true;
            } else {
                self.line.extend_from_slice(&available[..end]);
            }
            self.reader.consume(end);
            if newline.is_some() {
                return Ok(self.take());
            }
        }
    }

    fn take(&mut self) -> Input {
        let input = if self.overflow {
            Input::TooLong
        } else {
            let text = String::from_utf8_lossy(&self.line);
            Input::Line(text.trim_end_matches(['\r', '\n']).to_owned())
        };
        self.line.clear();
        self.overflow = false;
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_capped_lines() {
        let mut data = b"one\r\ntwo\n".to_vec();
        data.extend(vec![b'x'; MAX_INPUT + 1]);
        data.extend(b"\nlast");
        let mut reader = LineReader::new(data.as_slice());
        let mut got = Vec::new();
        loop {
            match reader.next().await.unwrap() {
                Input::Line(line) => got.push(line),
                Input::TooLong => got.push("<too long>".to_owned()),
                Input::End => break,
            }
        }
        assert_eq!(got, ["one", "two", "<too long>", "last"]);
    }

    #[test]
    fn control_characters_are_not_printed() {
        assert_eq!(printable("ok\x1b]0;pwned\x07\tend"), "ok?]0;pwned?\tend");
    }
}
