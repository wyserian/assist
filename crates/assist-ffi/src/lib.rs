//! The assist core for the Android app (PLAN.md Phase 4), through UniFFI: the same engine,
//! dialog, policy and scheduler as the desktop, built by `assistant::assemble`. Kotlin calls
//! [`Assistant::say`] and shows the answer; jobs run on the core's own tokio runtime.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use assist_core::assistant::{assemble, Host};
use assist_core::config::Config;
use assist_core::dialog::{Dialog, LiveEngine};
use assist_core::policy::Principal;
use assist_core::scheduler::Scheduler;
use assist_core::store::Store;

uniffi::setup_scaffolding!();

/// Threads for jobs; a phone runs few at a time.
const WORKERS: usize = 2;

/// Why the assistant could not start.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum AssistError {
    /// The config, data directory or engine failed.
    #[error("{0}")]
    Start(String),
}

/// What the assistant answers.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Answer {
    /// Lines to show or speak, in order.
    pub say: Vec<String>,
    /// Jobs it started.
    pub jobs: Vec<Job>,
    /// It waits for an answer (yes or no, a name, ...).
    pub waiting: bool,
}

/// A started job.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Job {
    /// Job id, for [`Assistant::output`].
    pub id: u64,
    /// What it runs, as read back.
    pub title: String,
}

/// One conversation with the core on this device.
#[derive(uniffi::Object)]
pub struct Assistant {
    runtime: tokio::runtime::Runtime,
    scheduler: Scheduler,
    engine: Arc<LiveEngine>,
    store: Store,
    dialog: Mutex<Dialog>,
    owner: Principal,
    home: PathBuf,
}

#[uniffi::export]
impl Assistant {
    /// Starts the core with `config_toml` (the contents of config.toml) and its data in
    /// `data_dir` (the app's private files directory).
    #[uniffi::constructor]
    pub fn new(config_toml: String, data_dir: String) -> Result<Arc<Self>, AssistError> {
        let failed = |err: &dyn std::fmt::Display| AssistError::Start(err.to_string());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .enable_all()
            .build()
            .map_err(|e| failed(&e))?;
        let config = Arc::new(Config::from_toml(&config_toml).map_err(|e| failed(&e))?);
        let home = PathBuf::from(data_dir);
        let host = Host {
            actions: assist_actions::builtin(),
            ..Host::default()
        };
        // The scheduler spawns its tasks on the runtime it is built in.
        let parts = {
            let _inside = runtime.enter();
            assemble(config, &home, host).map_err(|e| failed(&e))?
        };
        let owner = Principal::local("android");
        Ok(Arc::new(Self {
            dialog: Mutex::new(Dialog::new(parts.engine.get(), owner.clone())),
            runtime,
            scheduler: parts.scheduler,
            engine: parts.engine,
            store: parts.store,
            owner,
            home,
        }))
    }

    /// Handles what the user typed or said and starts the jobs it asks for.
    pub fn say(&self, text: String) -> Answer {
        let engine = self.engine.get();
        let reply = {
            let mut dialog = self.dialog.lock().unwrap_or_else(PoisonError::into_inner);
            dialog.use_engine(Arc::clone(&engine));
            dialog.handle(&self.owner, &text, Instant::now())
        };
        let waiting = self
            .dialog
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_waiting();
        let mut answer = Answer {
            say: reply.say,
            jobs: Vec::new(),
            waiting,
        };
        let _inside = self.runtime.enter();
        for pipe in reply.run {
            let run = engine.submission(pipe);
            match self
                .scheduler
                .submit_pipe(run, self.owner.clone(), self.home.clone())
            {
                Ok(ids) => answer.jobs.extend(ids.into_iter().map(|id| Job {
                    id,
                    title: self.scheduler.get(id).map(|j| j.title).unwrap_or_default(),
                })),
                Err(err) => answer.say.push(err.to_string()),
            }
        }
        answer
    }

    /// A job's last output lines and whether it has ended.
    pub fn output(&self, job: u64) -> JobOutput {
        let info = self.scheduler.get(job);
        JobOutput {
            lines: self.scheduler.tail(job).unwrap_or_default(),
            ended: info.as_ref().is_none_or(|j| j.state.is_finished()),
            state: info.map_or_else(|| "gone".to_owned(), |j| j.state.as_str().to_owned()),
        }
    }
}

/// What a job printed so far.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JobOutput {
    /// Its last lines, oldest first.
    pub lines: Vec<String>,
    /// `queued`, `running`, `done`, `failed`, ... or `gone` when it is no longer kept.
    pub state: String,
    /// It finished or is no longer kept: no more lines will come.
    pub ended: bool,
}

impl Drop for Assistant {
    fn drop(&mut self) {
        // Writes pending job history before the app's process goes away.
        self.store.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_answers_like_the_desktop() {
        let dir = std::env::temp_dir().join(format!("assist-ffi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let assistant = Assistant::new(String::new(), dir.display().to_string()).unwrap();
        let answer = assistant.say("remember that the car is on level 3".to_owned());
        assert_eq!(answer.jobs.len(), 1);
        for _ in 0..100 {
            if assistant.output(answer.jobs[0].id).ended {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let recalled = assistant.say("where is the car".to_owned());
        assert_eq!(recalled.say, ["The car is on level 3."]);
        drop(assistant);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
