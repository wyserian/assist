//! Schedules and triggers in the daemon (PLAN.md section 3.12): the clock, fact changes and
//! folder changes start saved pipes marked `unattended`. The rules live in core
//! (`assist_core::trigger`, `Engine::unattended`); this module reads the clock and the
//! file system and reports what happened on the daemon's console.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use assist_core::config::Config;
use assist_core::dialog::LiveEngine;
use assist_core::facts::Facts;
use assist_core::policy::Principal;
use assist_core::scheduler::Scheduler;
use assist_core::trigger::{FactEdge, Missed, Schedule};
use jiff::{SignedDuration, ZonedDateTime};
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// How often schedules and fact conditions are checked.
const TICK: Duration = Duration::from_secs(15);
/// A schedule this late counts as missed (the device slept), not just a slow tick.
const LATE: SignedDuration = SignedDuration::from_mins(2);
/// Quiet time after the last change in a folder before its trigger fires, so a copy of
/// many files fires once.
const SETTLE: Duration = Duration::from_secs(2);
/// Changes within this time after a folder trigger fired are ignored, so a pipe that
/// writes into the folder it watches does not run in a tight loop.
///
/// shortcut: a pipe that keeps writing into its own folder still runs every cooldown;
/// upgrade to ignoring changes until its jobs end when such pipes exist.
const COOLDOWN: Duration = Duration::from_secs(10);
/// Folder changes waiting for the runner; more are dropped (the next one settles anyway).
const EVENT_QUEUE: usize = 64;

/// What a trigger needs to start a pipe.
#[derive(Clone)]
pub struct Runner {
    /// The current engine.
    pub engine: Arc<LiveEngine>,
    /// Runs the pipe's jobs.
    pub scheduler: Scheduler,
    /// Current facts.
    pub facts: Facts,
    /// Working directory for the jobs.
    pub cwd: PathBuf,
}

/// Starts the schedules and triggers in `config`. Runs until the runtime stops.
pub fn start(config: &Config, runner: Runner) {
    let schedules: Vec<(Schedule, String, Missed)> = config
        .schedules
        .iter()
        .filter_map(|s| {
            let schedule = Schedule::parse(s.at.as_deref(), s.every.as_deref()).ok()?;
            Some((schedule, s.run.clone(), s.missed))
        })
        .collect();
    let edges: Vec<(FactEdge, String)> = config
        .triggers
        .iter()
        .filter_map(|t| {
            let condition = assist_core::condition::Condition::parse(t.on.as_deref()?).ok()?;
            Some((FactEdge::new(condition), t.run.clone()))
        })
        .collect();
    let folders: Vec<(PathBuf, String)> = config
        .triggers
        .iter()
        .filter_map(|t| Some((home(t.watch.as_deref()?), t.run.clone())))
        .collect();
    if !schedules.is_empty() || !edges.is_empty() {
        tokio::spawn(clock(schedules, edges, runner.clone()));
    }
    if !folders.is_empty() {
        tokio::spawn(watch(folders, runner));
    }
}

/// Fires schedules when due and fact triggers when their condition turns true.
async fn clock(
    schedules: Vec<(Schedule, String, Missed)>,
    mut edges: Vec<(FactEdge, String)>,
    runner: Runner,
) {
    let start = ZonedDateTime::now();
    let mut due: Vec<Option<ZonedDateTime>> = schedules
        .iter()
        .map(|(s, _, _)| s.next_after(&start))
        .collect();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let now = ZonedDateTime::now();
        for ((schedule, run, missed), due) in schedules.iter().zip(due.iter_mut()) {
            let Some(at) = due.as_ref().filter(|at| now >= **at) else {
                continue;
            };
            let late = now.timestamp().duration_since(at.timestamp()) > LATE;
            if late && *missed == Missed::Skip {
                println!(
                    "schedule: skipped {run}, missed at {} while asleep",
                    at.datetime()
                );
            } else {
                runner.fire(run, "schedule");
            }
            *due = schedule.next_after(&now);
        }
        for (edge, run) in &mut edges {
            if edge.check(&runner.facts) {
                runner.fire(run, "trigger");
            }
        }
    }
}

/// Fires a folder's trigger once its changes settle.
async fn watch(folders: Vec<(PathBuf, String)>, runner: Runner) {
    let (tx, mut rx) = mpsc::channel::<usize>(EVENT_QUEUE);
    let dirs: Vec<PathBuf> = folders.iter().map(|(dir, _)| dir.clone()).collect();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in &event.paths {
            if let Some(i) = dirs.iter().position(|dir| path.starts_with(dir)) {
                let _ = tx.try_send(i);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(err) => return println!("trigger: cannot watch folders: {err}"),
    };
    for (dir, run) in &folders {
        if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            println!("trigger: cannot watch {} for {run}: {err}", dir.display());
        }
    }
    let mut settle: Vec<Option<Instant>> = vec![None; folders.len()];
    let mut fired: Vec<Option<Instant>> = vec![None; folders.len()];
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            Some(i) = rx.recv() => {
                let cooling = fired.get(i).copied().flatten().is_some_and(|t| t.elapsed() < COOLDOWN);
                if let (Some(slot), false) = (settle.get_mut(i), cooling) {
                    *slot = Some(Instant::now() + SETTLE);
                }
            }
            _ = tick.tick() => {
                for (i, (_, run)) in folders.iter().enumerate() {
                    if settle[i].is_some_and(|at| at <= Instant::now()) {
                        settle[i] = None;
                        fired[i] = Some(Instant::now());
                        runner.fire(run, "trigger");
                    }
                }
            }
        }
    }
}

impl Runner {
    /// Starts saved pipe `run` if the unattended rules allow it, and says what happened.
    fn fire(&self, run: &str, source: &str) {
        let engine = self.engine.get();
        let principal = Principal::local(source);
        match engine.unattended(run, &principal) {
            Ok(Some(pipe)) => {
                let submission = engine.submission(pipe);
                match self
                    .scheduler
                    .submit_pipe(submission, principal, self.cwd.clone())
                {
                    Ok(ids) => {
                        let first = ids.first().copied().unwrap_or_default();
                        println!("{source}: started {run} (#{first})");
                    }
                    Err(err) => println!("{source}: cannot start {run}: {err}"),
                }
            }
            Ok(None) => println!("{source}: skipped {run}, its if does not hold"),
            Err(reason) => println!("{source}: not running {reason}"),
        }
    }
}

/// `~/x` as a path in the home folder.
fn home(path: &str) -> PathBuf {
    let path = path.trim();
    match (path.strip_prefix("~"), directories::BaseDirs::new()) {
        (Some(rest), Some(dirs)) => dirs.home_dir().join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_means_home() {
        let dirs = directories::BaseDirs::new().unwrap();
        assert_eq!(home("~/Downloads"), dirs.home_dir().join("Downloads"));
        assert_eq!(home("/tmp/x"), PathBuf::from("/tmp/x"));
    }
}
