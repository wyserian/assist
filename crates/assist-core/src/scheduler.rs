//! Jobs and lanes (PLAN.md §3.8).
//!
//! Every confirmed [`Plan`] becomes a job. Jobs run in parallel, limited per lane, each with
//! a timeout, a bounded output tail and a cancel token. State changes and output are
//! broadcast as [`JobEvent`]s and every state change is audited.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::action::{Action, ActionError, Chunk, JobCtx, Lane};
use crate::audit::{now_ms, Audit, AuditRecord};
use crate::config::Config;
use crate::dialog::{CommandInfo, Plan};
use crate::policy::{Principal, Trust};

/// Job number, unique per daemon run.
pub type JobId = u64;

const OUTPUT_BUFFER: usize = 256;
const EVENT_BUFFER: usize = 1024;

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a lane slot.
    Queued,
    /// Running.
    Running,
    /// Finished successfully.
    Done,
    /// The action reported an error.
    Failed,
    /// Stopped by the user.
    Cancelled,
    /// Stopped after `[jobs] timeout`.
    TimedOut,
}

impl JobState {
    /// Whether the job has ended.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

/// A job as listed by `jobs`.
#[derive(Debug, Clone)]
pub struct JobInfo {
    /// Job id.
    pub id: JobId,
    /// Command name.
    pub command: String,
    /// The resolved action as read back to the user.
    pub title: String,
    /// Who asked.
    pub principal: Principal,
    /// Lane it runs in.
    pub lane: Lane,
    /// Current state.
    pub state: JobState,
    /// When it was submitted.
    pub created: SystemTime,
    /// When it started running.
    pub started: Option<SystemTime>,
    /// When it ended.
    pub finished: Option<SystemTime>,
    /// Why it failed.
    pub error: Option<ActionError>,
    /// The last output lines, at most `[jobs] output_tail_kb`.
    pub tail: VecDeque<String>,
}

/// Something that happened to a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// The job changed state.
    State {
        /// Job id.
        id: JobId,
        /// New state.
        state: JobState,
    },
    /// The job produced output.
    Output {
        /// Job id.
        id: JobId,
        /// The output.
        chunk: Chunk,
    },
}

/// Why a plan could not be submitted.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    /// No action is registered for the plan's command.
    #[error("no action for command {0}")]
    UnknownCommand(String),
}

/// Runs jobs. Cheap to clone; clones share all state.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

struct Inner {
    actions: BTreeMap<String, Arc<dyn Action>>,
    config: Arc<Config>,
    lanes: BTreeMap<Lane, Arc<Semaphore>>,
    jobs: Mutex<BTreeMap<JobId, Slot>>,
    next_id: AtomicU64,
    events: broadcast::Sender<JobEvent>,
    audit: Option<Audit>,
}

struct Slot {
    info: JobInfo,
    cancel: CancellationToken,
    tail_bytes: usize,
}

impl Scheduler {
    /// A scheduler for `actions`; `audit` records every state change.
    pub fn new(actions: Vec<Arc<dyn Action>>, config: Arc<Config>, audit: Option<Audit>) -> Self {
        let lanes = BTreeMap::from([
            (
                Lane::Interactive,
                Arc::new(Semaphore::new(config.lanes.interactive)),
            ),
            (
                Lane::Background,
                Arc::new(Semaphore::new(config.lanes.background)),
            ),
        ]);
        let actions = actions
            .into_iter()
            .map(|action| (action.manifest().name.to_owned(), action))
            .collect();
        Self {
            inner: Arc::new(Inner {
                actions,
                config,
                lanes,
                jobs: Mutex::new(BTreeMap::new()),
                next_id: AtomicU64::new(1),
                events: broadcast::channel(EVENT_BUFFER).0,
                audit,
            }),
        }
    }

    /// The commands this scheduler can run, for the dialog engine.
    pub fn commands(&self) -> BTreeMap<String, CommandInfo> {
        self.inner
            .actions
            .iter()
            .map(|(name, action)| {
                let manifest = action.manifest();
                let info = CommandInfo {
                    capabilities: manifest.capabilities.to_vec(),
                    owner_defined: manifest.owner_defined,
                };
                (name.clone(), info)
            })
            .collect()
    }

    /// Queues a confirmed plan and returns at once. Must be called inside a tokio runtime.
    pub fn submit(
        &self,
        plan: Plan,
        principal: Principal,
        title: String,
        cwd: PathBuf,
    ) -> Result<JobId, SubmitError> {
        let inner = &self.inner;
        let action = inner
            .actions
            .get(&plan.command)
            .cloned()
            .ok_or_else(|| SubmitError::UnknownCommand(plan.command.clone()))?;
        let lane = action.lane(&plan, &inner.config);
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        let info = JobInfo {
            id,
            command: plan.command.clone(),
            title,
            principal: principal.clone(),
            lane,
            state: JobState::Queued,
            created: SystemTime::now(),
            started: None,
            finished: None,
            error: None,
            tail: VecDeque::new(),
        };
        inner.jobs().insert(
            id,
            Slot {
                info,
                cancel: cancel.clone(),
                tail_bytes: 0,
            },
        );
        inner.audit(id, "submitted");
        inner.emit(JobEvent::State {
            id,
            state: JobState::Queued,
        });
        let ctx = JobCtx {
            job: id,
            plan,
            principal,
            config: Arc::clone(&inner.config),
            cwd,
            cancel,
        };
        tokio::spawn(run_job(Arc::clone(inner), action, ctx, lane));
        Ok(id)
    }

    /// Stops a queued or running job. Returns false if it does not exist or already ended.
    pub fn cancel(&self, id: JobId) -> bool {
        match self.inner.jobs().get(&id) {
            Some(slot) if !slot.info.state.is_finished() => {
                slot.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Stops every queued and running job; returns how many.
    pub fn cancel_all(&self) -> usize {
        let jobs = self.inner.jobs();
        let active: Vec<&Slot> = jobs
            .values()
            .filter(|slot| !slot.info.state.is_finished())
            .collect();
        for slot in &active {
            slot.cancel.cancel();
        }
        active.len()
    }

    /// All jobs, oldest first.
    pub fn list(&self) -> Vec<JobInfo> {
        self.inner
            .jobs()
            .values()
            .map(|slot| slot.info.clone())
            .collect()
    }

    /// One job.
    pub fn get(&self, id: JobId) -> Option<JobInfo> {
        self.inner.jobs().get(&id).map(|slot| slot.info.clone())
    }

    /// Live job events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.inner.events.subscribe()
    }
}

async fn run_job(inner: Arc<Inner>, action: Arc<dyn Action>, ctx: JobCtx, lane: Lane) {
    let id = ctx.job;
    let cancel = ctx.cancel.clone();
    let Some(semaphore) = inner.lanes.get(&lane).cloned() else {
        return inner.finish(id, JobState::Failed, Some(ActionError::new("no such lane")));
    };
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        permit = semaphore.acquire_owned() => permit.ok(),
    };
    let Some(_permit) = permit else {
        return inner.finish(id, JobState::Cancelled, None);
    };
    inner.update(id, |info| {
        info.state = JobState::Running;
        info.started = Some(SystemTime::now());
    });
    inner.audit(id, "running");
    inner.emit(JobEvent::State {
        id,
        state: JobState::Running,
    });

    let (input_tx, input) = mpsc::channel(1);
    drop(input_tx);
    let (out, mut output) = mpsc::channel(OUTPUT_BUFFER);
    let mut work = action.run(ctx, input, out);
    let deadline = tokio::time::sleep(inner.config.jobs.timeout);
    tokio::pin!(deadline);
    let (state, error) = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break (JobState::Cancelled, None),
            _ = &mut deadline => break (JobState::TimedOut, None),
            Some(chunk) = output.recv() => inner.output(id, chunk),
            result = &mut work => {
                while let Ok(chunk) = output.try_recv() {
                    inner.output(id, chunk);
                }
                break match result {
                    Ok(()) => (JobState::Done, None),
                    Err(err) => (JobState::Failed, Some(err)),
                };
            }
        }
    };
    // Dropping the future stops the action and kills its processes if it is still running.
    drop(work);
    cancel.cancel();
    inner.finish(id, state, error);
}

impl Inner {
    fn jobs(&self) -> MutexGuard<'_, BTreeMap<JobId, Slot>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, id: JobId, change: impl FnOnce(&mut JobInfo)) {
        if let Some(slot) = self.jobs().get_mut(&id) {
            change(&mut slot.info);
        }
    }

    fn emit(&self, event: JobEvent) {
        // No subscribers is fine.
        let _ = self.events.send(event);
    }

    fn output(&self, id: JobId, chunk: Chunk) {
        let line = match &chunk {
            Chunk::Line(text) | Chunk::Info(text) => Some(text.clone()),
            Chunk::Error(text) => Some(format!("! {text}")),
            Chunk::Cwd(_) => None,
        };
        if let Some(line) = line {
            let cap = self.config.jobs.output_tail_kb * 1024;
            if let Some(slot) = self.jobs().get_mut(&id) {
                slot.tail_bytes += line.len();
                slot.info.tail.push_back(line);
                while slot.tail_bytes > cap {
                    match slot.info.tail.pop_front() {
                        Some(old) => slot.tail_bytes -= old.len(),
                        None => break,
                    }
                }
            }
        }
        self.emit(JobEvent::Output { id, chunk });
    }

    fn finish(&self, id: JobId, state: JobState, error: Option<ActionError>) {
        let message = error.as_ref().map(|e| e.message.clone());
        self.update(id, |info| {
            info.state = state;
            info.finished = Some(SystemTime::now());
            info.error = error;
        });
        self.audit_with(id, state.name(), message);
        self.emit(JobEvent::State { id, state });
    }

    fn audit(&self, id: JobId, event: &str) {
        self.audit_with(id, event, None);
    }

    fn audit_with(&self, id: JobId, event: &str, error: Option<String>) {
        let Some(audit) = &self.audit else { return };
        let Some(info) = self.jobs().get(&id).map(|slot| slot.info.clone()) else {
            return;
        };
        audit.record(AuditRecord {
            ts: now_ms(),
            event: event.to_owned(),
            job: id,
            command: info.command,
            title: info.title,
            source: info.principal.source,
            user: info.principal.user,
            trust: trust_name(info.principal.trust).to_owned(),
            error,
        });
    }
}

fn trust_name(trust: Trust) -> &'static str {
    match trust {
        Trust::Local => "local",
        Trust::OwnerRemote => "owner_remote",
        Trust::Voice => "voice",
        Trust::Untrusted => "untrusted",
        Trust::Autopilot => "autopilot",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{BoxFuture, Input, Manifest, Output};

    #[derive(Clone, Copy)]
    enum Behavior {
        Echo,
        Fail,
        Wait,
    }

    struct Fake {
        manifest: Manifest,
        behavior: Behavior,
    }

    impl Fake {
        fn action(name: &'static str, behavior: Behavior) -> Arc<dyn Action> {
            Arc::new(Self {
                manifest: Manifest {
                    name,
                    capabilities: &[],
                    owner_defined: false,
                    lane: Lane::Interactive,
                },
                behavior,
            })
        }
    }

    impl Action for Fake {
        fn manifest(&self) -> &Manifest {
            &self.manifest
        }

        fn run(
            &self,
            ctx: JobCtx,
            _input: Input,
            out: Output,
        ) -> BoxFuture<Result<(), ActionError>> {
            let behavior = self.behavior;
            Box::pin(async move {
                match behavior {
                    Behavior::Echo => {
                        for line in ["one", "two", "three"] {
                            let _ = out.send(Chunk::Line(line.to_owned())).await;
                        }
                        Ok(())
                    }
                    Behavior::Fail => Err(ActionError::exit(3)),
                    Behavior::Wait => {
                        ctx.cancel.cancelled().await;
                        Ok(())
                    }
                }
            })
        }
    }

    fn scheduler(config: &str) -> Scheduler {
        let actions = vec![
            Fake::action("echo", Behavior::Echo),
            Fake::action("fail", Behavior::Fail),
            Fake::action("wait", Behavior::Wait),
        ];
        Scheduler::new(actions, Arc::new(Config::from_toml(config).unwrap()), None)
    }

    fn submit(scheduler: &Scheduler, command: &str) -> JobId {
        let plan = Plan {
            command: command.to_owned(),
            slots: BTreeMap::new(),
            typed: false,
        };
        scheduler
            .submit(
                plan,
                Principal::local("test"),
                command.to_owned(),
                PathBuf::from("."),
            )
            .unwrap()
    }

    async fn finished(events: &mut broadcast::Receiver<JobEvent>, id: JobId) -> JobState {
        loop {
            if let Ok(JobEvent::State { id: got, state }) = events.recv().await {
                if got == id && state.is_finished() {
                    return state;
                }
            }
        }
    }

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn job_runs_and_keeps_output() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let id = submit(&s, "echo");
        assert_eq!(finished(&mut events, id).await, JobState::Done);
        let info = s.get(id).unwrap();
        assert_eq!(info.tail, ["one", "two", "three"]);
        assert!(info.started.is_some() && info.finished.is_some());
    }

    #[tokio::test]
    async fn failure_keeps_the_exit_code() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let id = submit(&s, "fail");
        assert_eq!(finished(&mut events, id).await, JobState::Failed);
        assert_eq!(s.get(id).unwrap().error.unwrap().exit, Some(3));
    }

    #[tokio::test]
    async fn lane_limit_queues_and_cancel_works_in_both_states() {
        let s = scheduler("[lanes]\ninteractive = 1\n");
        let mut events = s.subscribe();
        let first = submit(&s, "wait");
        let second = submit(&s, "wait");
        let third = submit(&s, "wait");
        settle().await;
        assert_eq!(s.get(first).unwrap().state, JobState::Running);
        assert_eq!(s.get(second).unwrap().state, JobState::Queued);

        assert!(s.cancel(third));
        assert_eq!(finished(&mut events, third).await, JobState::Cancelled);
        assert!(s.cancel(first));
        assert_eq!(finished(&mut events, first).await, JobState::Cancelled);
        settle().await;
        assert_eq!(s.get(second).unwrap().state, JobState::Running);
        assert_eq!(s.cancel_all(), 1);
        assert_eq!(finished(&mut events, second).await, JobState::Cancelled);
        assert!(!s.cancel(second));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_stops_the_job() {
        let s = scheduler("[jobs]\ntimeout = \"5s\"\n");
        let mut events = s.subscribe();
        let id = submit(&s, "wait");
        assert_eq!(finished(&mut events, id).await, JobState::TimedOut);
    }

    #[tokio::test]
    async fn output_tail_is_capped() {
        let s = scheduler("[jobs]\noutput_tail_kb = 0\n");
        let mut events = s.subscribe();
        let id = submit(&s, "echo");
        finished(&mut events, id).await;
        assert!(s.get(id).unwrap().tail.is_empty());
    }

    #[tokio::test]
    async fn unknown_command_is_rejected() {
        let s = scheduler("");
        let plan = Plan {
            command: "fly".to_owned(),
            slots: BTreeMap::new(),
            typed: false,
        };
        let err = s
            .submit(
                plan,
                Principal::local("test"),
                String::new(),
                PathBuf::from("."),
            )
            .unwrap_err();
        assert!(matches!(err, SubmitError::UnknownCommand(ref c) if c == "fly"));
        assert_eq!(s.commands().len(), 3);
    }

    #[tokio::test]
    async fn state_changes_are_audited() {
        let dir = std::env::temp_dir().join(format!("assist-sched-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let audit = Audit::open(&dir).unwrap();
        let config = Arc::new(Config::from_toml("").unwrap());
        let s = Scheduler::new(
            vec![Fake::action("echo", Behavior::Echo)],
            config,
            Some(audit),
        );
        let mut events = s.subscribe();
        let id = submit(&s, "echo");
        finished(&mut events, id).await;
        // Records are written on blocking threads; give them a moment.
        let mut lines = 0;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let text: String = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
                .collect();
            lines = text.lines().count();
            if lines >= 3 {
                break;
            }
        }
        assert_eq!(lines, 3, "submitted, running, done");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
