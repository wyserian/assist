//! Jobs and lanes (PLAN.md section 3.8).
//!
//! Every confirmed [`Plan`] becomes a job. Jobs run in parallel, limited per lane, each with
//! a timeout, a bounded output tail and a cancel token. A pipe is a group of jobs, one per
//! stage, whose lines flow from stage to stage; the group takes one lane slot, and stopping
//! any stage stops the pipe. State changes and output are
//! broadcast as [`JobEvent`]s and every state change is audited. Only the most recent
//! [`KEEP_FINISHED`] finished jobs stay in memory.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{broadcast, mpsc, watch, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::action::{Action, ActionError, Chunk, Input, JobCtx, Lane, Output};
use crate::audit::{now_ms, Audit, AuditRecord};
use crate::config::Config;
use crate::dialog::{CommandInfo, Plan};
use crate::policy::Principal;
use crate::secrets::Redactor;
use crate::store::Store;
use assist_proto::JobSummary;

/// Job number; unique across restarts when a [`Store`] is used.
pub type JobId = u64;

/// Finished jobs kept for `jobs` and `log`; older ones are dropped (the audit log keeps them).
pub const KEEP_FINISHED: usize = 100;

const OUTPUT_BUFFER: usize = 256;
/// Lines waiting between two pipe stages; a full buffer makes the earlier stage wait.
const PIPE_BUFFER: usize = 256;
/// Processes recorded per job; an action starts one, a few at most.
const MAX_PIDS: usize = 8;
/// Pause before a failed pipe runs again.
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const EVENT_BUFFER: usize = 1024;

pub use assist_proto::JobState;

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
    /// When it was submitted, Unix ms.
    pub created: u64,
    /// When it started running, Unix ms.
    pub started: Option<u64>,
    /// When it ended, Unix ms.
    pub finished: Option<u64>,
    /// Why it failed.
    pub error: Option<ActionError>,
    /// The first job of the pipe this job is a stage of.
    pub group: Option<JobId>,
}

impl JobInfo {
    /// The job as sent to clients.
    pub fn summary(&self) -> JobSummary {
        JobSummary {
            id: self.id,
            command: self.command.clone(),
            title: self.title.clone(),
            source: self.principal.source.clone(),
            lane: self.lane,
            state: self.state,
            created: self.created,
            started: self.started,
            finished: self.finished,
            error: self.error.as_ref().map(|e| e.message.clone()),
            group: self.group,
            cpu: None,
            memory: None,
        }
    }
}

/// Where job history goes; both are optional (tests and the offline shell can skip them).
#[derive(Debug, Clone, Default)]
pub struct Persistence {
    /// Hash-chained audit log of every state change.
    pub audit: Option<Audit>,
    /// Job history and full output logs.
    pub store: Option<Store>,
    /// Scrubs secret values from titles and output before anything is stored or shown.
    pub redactor: Option<Redactor>,
    /// Data directory handed to actions (undo records).
    pub data_dir: Option<PathBuf>,
    /// Pauses and measures job processes; without it, running jobs cannot be paused.
    pub processes: Option<Arc<dyn Processes>>,
}

/// What a job's processes use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// CPU in whole percent of the machine.
    pub cpu: u32,
    /// Resident memory in bytes.
    pub memory: u64,
}

/// Pauses, resumes and measures processes together with their descendants: the OS side of
/// job control (PLAN.md section 3.8), implemented outside core.
pub trait Processes: Send + Sync {
    /// Suspends the processes and their descendants.
    fn suspend(&self, roots: &[u32]) -> Result<(), String>;
    /// Resumes the processes and their descendants.
    fn resume(&self, roots: &[u32]) -> Result<(), String>;
    /// Use per group of root processes, in order; `None` for a group that has ended.
    /// Blocking: CPU is sampled over a short interval.
    fn usage(&self, groups: &[Vec<u32>]) -> Vec<Option<Usage>>;
}

impl std::fmt::Debug for dyn Processes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Processes")
    }
}

/// Why a job could not be paused or resumed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PauseError {
    /// No such job, or it has ended.
    #[error("no active job #{0}")]
    NotActive(JobId),
    /// It runs inside assist (no process to suspend).
    #[error("#{0} cannot be paused: it runs no process")]
    NoProcess(JobId),
    /// The OS refused.
    #[error("#{id}: {reason}")]
    Os {
        /// The job.
        id: JobId,
        /// What the OS said.
        reason: String,
    },
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
    /// A pipe with no stages, or a different number of titles.
    #[error("empty pipe")]
    EmptyPipe,
}

/// A confirmed pipe to run: stages with their titles, and for a saved pipe how to handle
/// failure. Retries and the `on_error` pipe run as new jobs.
///
/// shortcut: a client follows only the job ids `submit_pipe` returned, so it does not stream
/// retries and `on_error` jobs (`jobs` and the dashboard list them); upgrade when clients
/// follow job groups.
#[derive(Debug, Clone)]
pub struct Submission {
    /// Stages in order, each with its title.
    pub stages: Vec<(Plan, String)>,
    /// Runs again up to this many times when a stage fails.
    pub retry: u32,
    /// Runs when the last try failed; empty for nothing.
    pub on_error: Vec<(Plan, String)>,
}

impl From<Vec<(Plan, String)>> for Submission {
    fn from(stages: Vec<(Plan, String)>) -> Self {
        Self {
            stages,
            retry: 0,
            on_error: Vec::new(),
        }
    }
}

/// A stage resolved to its action, ready to become a job on every try.
type Stage = (Arc<dyn Action>, Plan, String);

/// Who runs a pipe and where; the same for every try.
#[derive(Clone)]
struct Origin {
    principal: Principal,
    cwd: PathBuf,
    lane: Lane,
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
    store: Option<Store>,
    redactor: Option<Redactor>,
    data_dir: Option<PathBuf>,
    processes: Option<Arc<dyn Processes>>,
}

struct Slot {
    info: JobInfo,
    cancel: CancellationToken,
    /// The last output lines, at most `[jobs] output_tail_kb` in total.
    tail: VecDeque<String>,
    tail_bytes: usize,
    /// Processes the action started.
    pids: Vec<u32>,
    /// True while the user holds the job's pipe; shared by its stages.
    hold: Arc<watch::Sender<bool>>,
}

impl Scheduler {
    /// A scheduler for `actions`, recording jobs to `persistence`. Job ids continue after the
    /// store's last job.
    pub fn new(
        actions: Vec<Arc<dyn Action>>,
        config: Arc<Config>,
        persistence: Persistence,
    ) -> Self {
        let next_id = persistence.store.as_ref().map_or(1, Store::next_id);
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
                next_id: AtomicU64::new(next_id),
                events: broadcast::channel(EVENT_BUFFER).0,
                audit: persistence.audit,
                store: persistence.store,
                redactor: persistence.redactor,
                data_dir: persistence.data_dir,
                processes: persistence.processes,
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
                    pipe: manifest.pipe,
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
        let ids = self.submit_pipe(vec![(plan, title)].into(), principal, cwd)?;
        ids.first().copied().ok_or(SubmitError::EmptyPipe)
    }

    /// Queues a confirmed pipe and returns the job ids of its stages (first try). The
    /// dialog has checked that each stage's input fits the previous output.
    pub fn submit_pipe(
        &self,
        run: Submission,
        principal: Principal,
        cwd: PathBuf,
    ) -> Result<Vec<JobId>, SubmitError> {
        let inner = &self.inner;
        let stages = inner.resolve(run.stages)?;
        let on_error = inner.resolve(run.on_error)?;
        let Some((first_action, first_plan, _)) = stages.first() else {
            return Err(SubmitError::EmptyPipe);
        };
        // The pipe runs in the lane of its first stage, the one that does the real work.
        let origin = Origin {
            principal,
            cwd,
            lane: first_action.lane(first_plan, &inner.config),
        };
        let group = CancellationToken::new();
        let first = inner.register(&stages, &origin, &group, None);
        let ids = first.iter().map(|(_, ctx)| ctx.job).collect();
        let retry = Retry {
            stages,
            tries: run.retry,
            on_error,
            origin,
        };
        tokio::spawn(run_pipe(Arc::clone(inner), first, group, retry));
        Ok(ids)
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

    /// A job's last output lines, oldest first.
    pub fn tail(&self, id: JobId) -> Option<Vec<String>> {
        self.inner
            .jobs()
            .get(&id)
            .map(|slot| slot.tail.iter().cloned().collect())
    }

    /// How many jobs are queued or running.
    pub fn active(&self) -> usize {
        self.inner
            .jobs()
            .values()
            .filter(|slot| !slot.info.state.is_finished())
            .count()
    }

    /// Pauses the pipe `id` belongs to: running stages' processes are suspended, queued
    /// stages wait. Pausing a paused pipe is fine.
    pub fn pause(&self, id: JobId) -> Result<(), PauseError> {
        self.hold(id, true)
    }

    /// Resumes a paused pipe.
    pub fn resume(&self, id: JobId) -> Result<(), PauseError> {
        self.hold(id, false)
    }

    fn hold(&self, id: JobId, pause: bool) -> Result<(), PauseError> {
        let inner = &self.inner;
        let (ids, hold, running, pids) = {
            let jobs = inner.jobs();
            let slot = jobs
                .get(&id)
                .filter(|s| !s.info.state.is_finished())
                .ok_or(PauseError::NotActive(id))?;
            let group = slot.info.group.unwrap_or(id);
            let stages: Vec<&Slot> = jobs
                .values()
                .filter(|s| s.info.group.unwrap_or(s.info.id) == group)
                .filter(|s| !s.info.state.is_finished())
                .collect();
            let running = stages.iter().any(|s| s.info.started.is_some());
            let pids: Vec<u32> = stages.iter().flat_map(|s| s.pids.iter().copied()).collect();
            let ids: Vec<JobId> = stages.iter().map(|s| s.info.id).collect();
            (ids, Arc::clone(&slot.hold), running, pids)
        };
        if *hold.borrow() == pause {
            return Ok(());
        }
        if running {
            let processes = inner.processes.as_ref().filter(|_| !pids.is_empty());
            let processes = processes.ok_or(PauseError::NoProcess(id))?;
            let done = if pause {
                processes.suspend(&pids)
            } else {
                processes.resume(&pids)
            };
            done.map_err(|reason| PauseError::Os { id, reason })?;
        }
        hold.send_replace(pause);
        for stage in ids {
            let mut started = false;
            inner.update(stage, |info| {
                started = info.started.is_some();
                info.state = match (pause, started) {
                    (true, _) => JobState::Paused,
                    (false, true) => JobState::Running,
                    (false, false) => JobState::Queued,
                };
            });
            let (event, state) = match (pause, started) {
                (true, _) => ("paused", JobState::Paused),
                (false, true) => ("resumed", JobState::Running),
                (false, false) => ("resumed", JobState::Queued),
            };
            inner.audit(stage, event);
            inner.emit(JobEvent::State { id: stage, state });
        }
        Ok(())
    }

    /// CPU and memory of running jobs' processes. Blocking: CPU is sampled over a short
    /// interval, so call it off the async runtime.
    pub fn usage(&self, ids: &[JobId]) -> BTreeMap<JobId, Usage> {
        let Some(processes) = &self.inner.processes else {
            return BTreeMap::new();
        };
        let measured: Vec<(JobId, Vec<u32>)> = {
            let jobs = self.inner.jobs();
            ids.iter()
                .filter_map(|id| {
                    let slot = jobs.get(id)?;
                    let active = matches!(slot.info.state, JobState::Running | JobState::Paused);
                    (active && !slot.pids.is_empty()).then(|| (*id, slot.pids.clone()))
                })
                .collect()
        };
        let groups: Vec<Vec<u32>> = measured.iter().map(|(_, pids)| pids.clone()).collect();
        measured
            .iter()
            .zip(processes.usage(&groups))
            .filter_map(|((id, _), usage)| Some((*id, usage?)))
            .collect()
    }

    /// Live job events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.inner.events.subscribe()
    }
}

/// What to do when a pipe fails.
struct Retry {
    stages: Vec<Stage>,
    tries: u32,
    on_error: Vec<Stage>,
    origin: Origin,
}

impl crate::dialog::Preview for Scheduler {
    fn preview(&self, plan: &Plan) -> Option<Result<Vec<String>, String>> {
        let inner = &self.inner;
        inner
            .actions
            .get(&plan.command)?
            .preview(plan, &inner.config, inner.data_dir.as_deref())
    }
}

/// Runs a pipe once its lane has room, holding one slot for the whole group (so a long
/// pipe never waits on itself), its retries and its `on_error` pipe.
async fn run_pipe(
    inner: Arc<Inner>,
    first: Vec<(Arc<dyn Action>, JobCtx)>,
    group: CancellationToken,
    retry: Retry,
) {
    let semaphore = inner.lanes.get(&retry.origin.lane).cloned();
    let mut held = first.first().map(|(_, ctx)| inner.hold_of(ctx.job));
    let permit = match semaphore {
        Some(semaphore) => tokio::select! {
            biased;
            _ = group.cancelled() => None,
            permit = async {
                // A pipe paused while queued waits before it takes a slot.
                if let Some(held) = held.as_mut() {
                    let _ = held.wait_for(|held| !held).await;
                }
                semaphore.acquire_owned().await
            } => permit.ok(),
        },
        None => None,
    };
    let Some(_permit) = permit else {
        for (_, ctx) in &first {
            inner.finish(ctx.job, JobState::Cancelled, None);
        }
        return;
    };
    let mut failed = run_stages(&inner, first, group).await;
    for attempt in 1..=retry.tries {
        if !failed {
            return;
        }
        tokio::time::sleep(RETRY_DELAY).await;
        let group = CancellationToken::new();
        let stages = inner.register(&retry.stages, &retry.origin, &group, Some(attempt));
        failed = run_stages(&inner, stages, group).await;
    }
    if failed && !retry.on_error.is_empty() {
        let group = CancellationToken::new();
        let stages = inner.register(&retry.on_error, &retry.origin, &group, None);
        run_stages(&inner, stages, group).await;
    }
}

/// Runs stages joined by channels; true if one failed or timed out (not when stopped).
async fn run_stages(
    inner: &Arc<Inner>,
    stages: Vec<(Arc<dyn Action>, JobCtx)>,
    group: CancellationToken,
) -> bool {
    let mut tasks = JoinSet::new();
    let (closed, mut input) = mpsc::channel(1);
    drop(closed);
    let last = stages.len().saturating_sub(1);
    for (i, (action, ctx)) in stages.into_iter().enumerate() {
        let (next, next_input) = if i < last {
            let (tx, rx) = mpsc::channel(PIPE_BUFFER);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let stage = run_stage(Arc::clone(inner), action, ctx, input, next, group.clone());
        tasks.spawn(stage);
        if let Some(rx) = next_input {
            input = rx;
        } else {
            break;
        }
    }
    let mut failed = false;
    while let Some(state) = tasks.join_next().await {
        failed |= matches!(state, Ok(JobState::Failed | JobState::TimedOut));
    }
    failed
}

/// Runs one stage. Its lines go to `next` when there is a next stage, else to the job's
/// output. A failed or timed-out stage stops the rest of the pipe.
async fn run_stage(
    inner: Arc<Inner>,
    action: Arc<dyn Action>,
    ctx: JobCtx,
    input: Input,
    next: Option<Output>,
    group: CancellationToken,
) -> JobState {
    let id = ctx.job;
    let cancel = ctx.cancel.clone();
    inner.update(id, |info| {
        info.state = JobState::Running;
        info.started = Some(now_ms());
    });
    inner.audit(id, "running");
    inner.emit(JobEvent::State {
        id,
        state: JobState::Running,
    });

    let (out, mut output) = mpsc::channel(OUTPUT_BUFFER);
    let mut held = inner.hold_of(id);
    let mut work = action.run(ctx, input, out);
    let deadline = tokio::time::sleep(inner.config.jobs.timeout);
    tokio::pin!(deadline);
    // The timeout clock stops while the job is paused.
    let mut paused_at: Option<tokio::time::Instant> = None;
    let (state, error) = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break (JobState::Cancelled, None),
            Ok(()) = held.changed() => {
                if *held.borrow_and_update() {
                    paused_at.get_or_insert_with(tokio::time::Instant::now);
                } else if let Some(at) = paused_at.take() {
                    let until = deadline.deadline() + at.elapsed();
                    deadline.as_mut().reset(until);
                }
            }
            _ = &mut deadline, if paused_at.is_none() => break (JobState::TimedOut, None),
            Some(chunk) = output.recv() => {
                if !pass(&inner, id, next.as_ref(), chunk).await {
                    // The next stage stopped reading (`head` is done): this stage is done.
                    break (JobState::Done, None);
                }
            }
            result = &mut work => {
                while let Ok(chunk) = output.try_recv() {
                    if !pass(&inner, id, next.as_ref(), chunk).await {
                        break;
                    }
                }
                break match result {
                    Ok(()) => (JobState::Done, None),
                    Err(err) => (JobState::Failed, Some(err)),
                };
            }
        }
    };
    // Dropping the future stops the action and kills its processes if it is still running;
    // dropping `next` ends the next stage's input.
    drop(work);
    // A failure stops the later stages before their input ends, so they end Cancelled
    // instead of finishing on partial input.
    if matches!(state, JobState::Failed | JobState::TimedOut) {
        group.cancel();
    }
    drop(next);
    cancel.cancel();
    inner.finish(id, state, error);
    state
}

/// Hands a chunk on: lines to the next stage when there is one, everything else to the
/// job's own output. `false` once the next stage is gone.
async fn pass(inner: &Inner, id: JobId, next: Option<&Output>, chunk: Chunk) -> bool {
    match (next, chunk) {
        (Some(next), Chunk::Line(line)) => next.send(Chunk::Line(line)).await.is_ok(),
        (_, chunk) => {
            inner.output(id, chunk);
            true
        }
    }
}

impl Inner {
    /// Each stage's action; fails on a command no action runs.
    fn resolve(&self, stages: Vec<(Plan, String)>) -> Result<Vec<Stage>, SubmitError> {
        stages
            .into_iter()
            .map(|(plan, title)| match self.actions.get(&plan.command) {
                Some(action) => Ok((Arc::clone(action), plan, title)),
                None => Err(SubmitError::UnknownCommand(plan.command)),
            })
            .collect()
    }

    /// New queued jobs for one try of `stages`, sharing `group` (stopping any stage stops
    /// them all). `attempt` marks a retry in the titles.
    fn register(
        &self,
        stages: &[Stage],
        origin: &Origin,
        group: &CancellationToken,
        attempt: Option<u32>,
    ) -> Vec<(Arc<dyn Action>, JobCtx)> {
        let piped = stages.len() > 1;
        let hold = Arc::new(watch::Sender::new(false));
        let mut group_id = None;
        let mut out = Vec::with_capacity(stages.len());
        for (action, plan, title) in stages {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let first = *group_id.get_or_insert(id);
            let title = match attempt {
                Some(n) => format!("{title} (retry {n})"),
                None => title.clone(),
            };
            let info = JobInfo {
                id,
                command: plan.command.clone(),
                title: self.redact(&title).into_owned(),
                principal: origin.principal.clone(),
                lane: origin.lane,
                state: JobState::Queued,
                created: now_ms(),
                started: None,
                finished: None,
                error: None,
                group: piped.then_some(first),
            };
            if let Some(store) = &self.store {
                store.insert(&info);
            }
            self.jobs().insert(
                id,
                Slot {
                    info,
                    cancel: group.clone(),
                    tail: VecDeque::new(),
                    tail_bytes: 0,
                    pids: Vec::new(),
                    hold: Arc::clone(&hold),
                },
            );
            self.audit(id, "submitted");
            self.emit(JobEvent::State {
                id,
                state: JobState::Queued,
            });
            let ctx = JobCtx {
                job: id,
                plan: plan.clone(),
                principal: origin.principal.clone(),
                config: Arc::clone(&self.config),
                cwd: origin.cwd.clone(),
                cancel: group.child_token(),
                data_dir: self.data_dir.clone(),
            };
            out.push((Arc::clone(action), ctx));
        }
        out
    }

    fn jobs(&self) -> MutexGuard<'_, BTreeMap<JobId, Slot>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, id: JobId, change: impl FnOnce(&mut JobInfo)) {
        if let Some(slot) = self.jobs().get_mut(&id) {
            change(&mut slot.info);
            if let Some(store) = &self.store {
                store.update(&slot.info);
            }
        }
    }

    fn emit(&self, event: JobEvent) {
        // No subscribers is fine.
        let _ = self.events.send(event);
    }

    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match &self.redactor {
            Some(redactor) => redactor.redact(text),
            None => Cow::Borrowed(text),
        }
    }

    /// The hold flag of `id`'s pipe; never held when the job is gone.
    fn hold_of(&self, id: JobId) -> watch::Receiver<bool> {
        self.jobs()
            .get(&id)
            .map_or_else(|| watch::channel(false).1, |slot| slot.hold.subscribe())
    }

    fn output(&self, id: JobId, chunk: Chunk) {
        if let Chunk::Process(pid) = chunk {
            if let Some(slot) = self.jobs().get_mut(&id) {
                if slot.pids.len() < MAX_PIDS {
                    slot.pids.push(pid);
                }
            }
            return;
        }
        let chunk = match chunk {
            Chunk::Line(text) => Chunk::Line(self.redact(&text).into_owned()),
            Chunk::Error(text) => Chunk::Error(self.redact(&text).into_owned()),
            Chunk::Info(text) => Chunk::Info(self.redact(&text).into_owned()),
            other => other,
        };
        let line = match &chunk {
            Chunk::Line(text) | Chunk::Info(text) => Some(text.clone()),
            Chunk::Error(text) => Some(format!("! {text}")),
            Chunk::Cwd(_) | Chunk::Process(_) => None,
        };
        if let Some(line) = line {
            if let Some(store) = &self.store {
                store.output(id, &line);
            }
            let cap = self.config.jobs.output_tail_kb * 1024;
            if let Some(slot) = self.jobs().get_mut(&id) {
                slot.tail_bytes += line.len();
                slot.tail.push_back(line);
                while slot.tail_bytes > cap {
                    match slot.tail.pop_front() {
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
            info.finished = Some(now_ms());
            info.error = error;
        });
        self.audit_with(id, state.as_str(), message);
        self.emit(JobEvent::State { id, state });
        self.prune();
    }

    /// Drops the oldest finished jobs beyond [`KEEP_FINISHED`].
    fn prune(&self) {
        let mut jobs = self.jobs();
        let finished: Vec<JobId> = jobs
            .iter()
            .filter(|(_, slot)| slot.info.state.is_finished())
            .map(|(id, _)| *id)
            .collect();
        let excess = finished.len().saturating_sub(KEEP_FINISHED);
        for id in finished.into_iter().take(excess) {
            jobs.remove(&id);
        }
    }

    fn audit(&self, id: JobId, event: &str) {
        self.audit_with(id, event, None);
    }

    fn audit_with(&self, id: JobId, event: &str, error: Option<String>) {
        let Some(audit) = &self.audit else { return };
        let record = {
            let jobs = self.jobs();
            let Some(info) = jobs.get(&id).map(|slot| &slot.info) else {
                return;
            };
            AuditRecord {
                ts: now_ms(),
                event: event.to_owned(),
                job: id,
                command: info.command.clone(),
                title: info.title.clone(),
                source: info.principal.source.clone(),
                user: info.principal.user.clone(),
                trust: info.principal.trust.as_str().to_owned(),
                error,
            }
        };
        audit.record(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{BoxFuture, Input, Manifest, Output, PipeShape};

    #[derive(Clone, Copy)]
    enum Behavior {
        Echo,
        Fail,
        Wait,
        Leak,
        /// Numbered lines until the next stage stops reading.
        Endless,
        /// Transform: input lines in upper case.
        Upper,
        /// Transform: the first input line only.
        First,
        /// Reports process 42, then works for 30 s.
        Slow,
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
                    pipe: match behavior {
                        Behavior::Upper | Behavior::First => {
                            PipeShape::transform(crate::action::DataKind::Lines, None)
                        }
                        _ => PipeShape::SOURCE,
                    },
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
            mut input: Input,
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
                    Behavior::Leak => {
                        let line = "token=sk-test-secret-value".to_owned();
                        let _ = out.send(Chunk::Line(line)).await;
                        Ok(())
                    }
                    Behavior::Wait => {
                        ctx.cancel.cancelled().await;
                        Ok(())
                    }
                    Behavior::Endless => {
                        for i in 0.. {
                            if out.send(Chunk::Line(i.to_string())).await.is_err() {
                                break;
                            }
                        }
                        Ok(())
                    }
                    Behavior::Upper => {
                        while let Some(Chunk::Line(line)) = input.recv().await {
                            let _ = out.send(Chunk::Line(line.to_uppercase())).await;
                        }
                        Ok(())
                    }
                    Behavior::First => {
                        if let Some(chunk) = input.recv().await {
                            let _ = out.send(chunk).await;
                        }
                        Ok(())
                    }
                    Behavior::Slow => {
                        let _ = out.send(Chunk::Process(42)).await;
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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
            Fake::action("endless", Behavior::Endless),
            Fake::action("upper", Behavior::Upper),
            Fake::action("first", Behavior::First),
            Fake::action("slow", Behavior::Slow),
        ];
        Scheduler::new(
            actions,
            Arc::new(Config::from_toml(config).unwrap()),
            Persistence::default(),
        )
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

    fn pipe(scheduler: &Scheduler, commands: &[&str]) -> Vec<JobId> {
        let stages: Vec<(Plan, String)> = commands
            .iter()
            .map(|command| {
                let plan = Plan {
                    command: (*command).to_owned(),
                    slots: BTreeMap::new(),
                    typed: false,
                };
                (plan, (*command).to_owned())
            })
            .collect();
        scheduler
            .submit_pipe(
                Submission::from(stages),
                Principal::local("test"),
                PathBuf::from("."),
            )
            .unwrap()
    }

    /// Records what was suspended and resumed.
    #[derive(Default)]
    struct FakeProcesses(Mutex<Vec<String>>);

    impl Processes for FakeProcesses {
        fn suspend(&self, roots: &[u32]) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("suspend {roots:?}"));
            Ok(())
        }
        fn resume(&self, roots: &[u32]) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("resume {roots:?}"));
            Ok(())
        }
        fn usage(&self, groups: &[Vec<u32>]) -> Vec<Option<Usage>> {
            groups
                .iter()
                .map(|_| {
                    Some(Usage {
                        cpu: 7,
                        memory: 1 << 20,
                    })
                })
                .collect()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn paused_jobs_keep_their_time_and_resume() {
        let processes = Arc::new(FakeProcesses::default());
        let mut s = scheduler(
            "[jobs]
timeout = \"10s\"
",
        );
        Arc::get_mut(&mut s.inner).unwrap().processes = Some(processes.clone());
        let mut events = s.subscribe();
        let id = submit(&s, "slow");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        s.pause(id).unwrap();
        assert_eq!(s.get(id).unwrap().state, JobState::Paused);
        assert_eq!(
            s.usage(&[id])[&id],
            Usage {
                cpu: 7,
                memory: 1 << 20
            }
        );
        // Far past the 10 s timeout while paused: still paused, not timed out.
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        assert_eq!(s.get(id).unwrap().state, JobState::Paused);
        s.resume(id).unwrap();
        assert_eq!(finished(&mut events, id).await, JobState::Done);
        assert_eq!(
            *processes.0.lock().unwrap(),
            ["suspend [42]", "resume [42]"]
        );
    }

    #[tokio::test]
    async fn a_job_without_a_process_cannot_be_paused() {
        let mut s = scheduler("");
        Arc::get_mut(&mut s.inner).unwrap().processes = Some(Arc::new(FakeProcesses::default()));
        let id = submit(&s, "wait");
        settle().await;
        assert_eq!(s.pause(id), Err(PauseError::NoProcess(id)));
        assert_eq!(s.pause(9999), Err(PauseError::NotActive(9999)));
        s.cancel(id);
    }

    fn stage(command: &str) -> (Plan, String) {
        let plan = Plan {
            command: command.to_owned(),
            slots: BTreeMap::new(),
            typed: false,
        };
        (plan, command.to_owned())
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_pipe_retries_then_runs_on_error() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let run = Submission {
            stages: vec![stage("fail")],
            retry: 1,
            on_error: vec![stage("echo")],
        };
        let ids = s
            .submit_pipe(run, Principal::local("test"), PathBuf::from("."))
            .unwrap();
        let (retry, on_error) = (ids[0] + 1, ids[0] + 2);
        assert_eq!(finished(&mut events, ids[0]).await, JobState::Failed);
        assert_eq!(finished(&mut events, retry).await, JobState::Failed);
        assert_eq!(finished(&mut events, on_error).await, JobState::Done);
        settle().await;
        assert_eq!(s.get(retry).unwrap().title, "fail (retry 1)");
        assert_eq!(s.tail(on_error).unwrap(), ["one", "two", "three"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pipe_that_works_runs_once() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let run = Submission {
            stages: vec![stage("echo")],
            retry: 2,
            on_error: vec![stage("echo")],
        };
        let ids = s
            .submit_pipe(run, Principal::local("test"), PathBuf::from("."))
            .unwrap();
        assert_eq!(finished(&mut events, ids[0]).await, JobState::Done);
        tokio::time::sleep(RETRY_DELAY * 4).await;
        assert_eq!(s.list().len(), 1);
    }

    #[tokio::test]
    async fn pipe_lines_flow_to_the_last_stage() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let ids = pipe(&s, &["echo", "upper"]);
        assert_eq!(finished(&mut events, ids[1]).await, JobState::Done);
        settle().await;
        assert_eq!(s.tail(ids[1]).unwrap(), ["ONE", "TWO", "THREE"]);
        assert!(s.tail(ids[0]).unwrap().is_empty());
        assert_eq!(s.get(ids[0]).unwrap().state, JobState::Done);
        assert_eq!(s.get(ids[1]).unwrap().group, Some(ids[0]));
    }

    #[tokio::test]
    async fn a_stage_that_stops_reading_ends_the_ones_before() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let ids = pipe(&s, &["endless", "first"]);
        assert_eq!(finished(&mut events, ids[0]).await, JobState::Done);
        settle().await;
        assert_eq!(s.tail(ids[1]).unwrap(), ["0"]);
        assert_eq!(s.active(), 0);
    }

    #[tokio::test]
    async fn a_failed_stage_cancels_the_rest_and_stop_stops_all() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let ids = pipe(&s, &["fail", "upper"]);
        assert_eq!(finished(&mut events, ids[1]).await, JobState::Cancelled);
        assert_eq!(s.get(ids[0]).unwrap().state, JobState::Failed);

        let ids = pipe(&s, &["wait", "upper"]);
        settle().await;
        assert!(s.cancel(ids[1]));
        assert_eq!(finished(&mut events, ids[0]).await, JobState::Cancelled);
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
        assert_eq!(s.tail(id).unwrap(), ["one", "two", "three"]);
        assert!(info.started.is_some() && info.finished.is_some());
        assert_eq!(s.active(), 0);
    }

    #[tokio::test]
    async fn old_finished_jobs_are_dropped() {
        let s = scheduler("");
        let mut events = s.subscribe();
        let ids: Vec<JobId> = (0..KEEP_FINISHED + 5).map(|_| submit(&s, "echo")).collect();
        while s.active() > 0 {
            let _ = events.recv().await;
        }
        let kept = s.list();
        assert_eq!(kept.len(), KEEP_FINISHED);
        assert_eq!(kept.first().map(|job| job.id), Some(ids[5]));
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
        assert!(s.tail(id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn secrets_are_redacted_from_titles_and_output() {
        let redactor = Redactor::default();
        redactor.add("api", "sk-test-secret-value");
        let config = Arc::new(Config::from_toml("").unwrap());
        let s = Scheduler::new(
            vec![Fake::action("leak", Behavior::Leak)],
            config,
            Persistence {
                redactor: Some(redactor),
                ..Persistence::default()
            },
        );
        let mut events = s.subscribe();
        let plan = Plan {
            command: "leak".to_owned(),
            slots: BTreeMap::new(),
            typed: false,
        };
        let title = "$ curl -H sk-test-secret-value".to_owned();
        let id = s
            .submit(plan, Principal::local("test"), title, PathBuf::from("."))
            .unwrap();
        let mut lines = Vec::new();
        loop {
            match events.recv().await.unwrap() {
                JobEvent::Output {
                    chunk: Chunk::Line(text),
                    ..
                } => lines.push(text),
                JobEvent::State { state, .. } if state.is_finished() => break,
                _ => {}
            }
        }
        assert_eq!(lines, ["token=[secret:api]"]);
        assert_eq!(s.tail(id).unwrap(), ["token=[secret:api]"]);
        assert_eq!(s.get(id).unwrap().title, "$ curl -H [secret:api]");
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
        assert_eq!(s.commands().len(), 7);
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
            Persistence {
                audit: Some(audit),
                ..Persistence::default()
            },
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
