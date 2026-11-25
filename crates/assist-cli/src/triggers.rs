//! Schedules, triggers and autopilot goals in the daemon (PLAN.md sections 3.12, 3.18): the
//! clock, fact changes and folder changes start saved pipes marked `unattended`, or run a
//! goal inside its budget. The rules live in core (`assist_core::trigger`,
//! `Engine::unattended`, `Engine::autopilot`); this module reads the clock and the file
//! system, keeps goal budgets and reports what happened on the daemon's console.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use assist_core::autopilot;
use assist_core::config::Config;
use assist_core::dialog::LiveEngine;
use assist_core::facts::Facts;
use assist_core::policy::{Principal, Trust};
use assist_core::scheduler::Scheduler;
use assist_core::trigger::{FactEdge, Missed, Schedule};
use assist_proto::Notice;
use jiff::{SignedDuration, ZonedDateTime};
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc};

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
    /// The config the goals come from.
    pub config: Arc<Config>,
    /// Data directory: autopilot state.
    pub data: PathBuf,
    /// Notices to clients when a goal pauses.
    pub notices: broadcast::Sender<Notice>,
}

/// What a trigger starts.
#[derive(Debug, Clone)]
enum Target {
    /// A saved pipe marked `unattended`.
    Pipe(String),
    /// An autopilot goal.
    Goal(String),
}

/// Starts the schedules and triggers in `config`. Runs until the runtime stops.
pub fn start(config: &Config, runner: Runner) {
    let mut schedules: Vec<(Schedule, Target, Missed)> = config
        .schedules
        .iter()
        .filter_map(|s| {
            let schedule = Schedule::parse(s.at.as_deref(), s.every.as_deref()).ok()?;
            Some((schedule, Target::Pipe(s.run.clone()), s.missed))
        })
        .collect();
    let mut edges: Vec<(FactEdge, Target)> = config
        .triggers
        .iter()
        .filter_map(|t| edge(t.on.as_deref()?, Target::Pipe(t.run.clone())))
        .collect();
    let mut folders: Vec<(PathBuf, Target)> = config
        .triggers
        .iter()
        .filter_map(|t| Some(folder(t.watch.as_deref()?, Target::Pipe(t.run.clone()))))
        .collect();
    if config.autopilot.enabled {
        for goal in &config.autopilot.goals {
            let target = Target::Goal(goal.name.clone());
            let t = &goal.trigger;
            if let Ok(schedule) = Schedule::parse(t.at.as_deref(), t.every.as_deref()) {
                schedules.push((schedule, target.clone(), t.missed));
            }
            edges.extend(t.on.as_deref().and_then(|on| edge(on, target.clone())));
            folders.extend(t.watch.as_deref().map(|dir| folder(dir, target.clone())));
        }
    }
    if !schedules.is_empty() || !edges.is_empty() {
        tokio::spawn(clock(schedules, edges, runner.clone()));
    }
    if !folders.is_empty() {
        tokio::spawn(watch(folders, runner));
    }
}

fn edge(on: &str, target: Target) -> Option<(FactEdge, Target)> {
    let condition = assist_core::condition::Condition::parse(on).ok()?;
    Some((FactEdge::new(condition), target))
}

fn folder(dir: &str, target: Target) -> (PathBuf, Target) {
    (assist_actions::expand_home(dir), target)
}

/// Fires schedules when due and fact triggers when their condition turns true.
async fn clock(
    schedules: Vec<(Schedule, Target, Missed)>,
    mut edges: Vec<(FactEdge, Target)>,
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
                    "schedule: skipped {run:?}, missed at {} while asleep",
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
async fn watch(folders: Vec<(PathBuf, Target)>, runner: Runner) {
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
            println!("trigger: cannot watch {} for {run:?}: {err}", dir.display());
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
    fn fire(&self, target: &Target, source: &str) {
        match target {
            Target::Pipe(run) => self.fire_pipe(run, source),
            Target::Goal(name) => self.fire_goal(name),
        }
    }

    /// Starts saved pipe `run` if the unattended rules allow it, and says what happened.
    fn fire_pipe(&self, run: &str, source: &str) {
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

    /// Runs goal `name` inside its budget; a refusal or an overrun pauses the goal.
    fn fire_goal(&self, name: &str) {
        let Some(goal) = self.config.autopilot.goals.iter().find(|g| g.name == name) else {
            return;
        };
        let today = ZonedDateTime::now().date().to_string();
        let state = autopilot::load(&self.data);
        if state.off || state.paused.contains(name) {
            return println!("autopilot: {name} is paused");
        }
        if today > goal.expires {
            return println!("autopilot: {name} expired on {}", goal.expires);
        }
        if state.used_on(name, &today) >= goal.budget.jobs_per_day {
            return self.pause(name, "it used its jobs for today");
        }
        let principal = Principal {
            source: format!("autopilot:{name}"),
            user: "owner".to_owned(),
            trust: Trust::Autopilot,
        };
        let engine = self.engine.get();
        let pipe = match engine.autopilot(name, &principal) {
            Ok(Some(pipe)) => pipe,
            Ok(None) => return println!("autopilot: skipped {name}, its if does not hold"),
            Err(reason) => return self.pause(name, &reason),
        };
        let submission = engine.submission(pipe);
        let ids = match self
            .scheduler
            .submit_pipe(submission, principal, self.cwd.clone())
        {
            Ok(ids) => ids,
            Err(err) => return println!("autopilot: cannot start {name}: {err}"),
        };
        println!(
            "autopilot: started {name} (#{})",
            ids.first().copied().unwrap_or_default()
        );
        // shortcut: retries and the on_error pipe are neither counted nor timed; count
        // them when goals use saved pipes with retry.
        let started = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        let counted = autopilot::update(&self.data, |s| {
            let used = s.used_on(name, &today);
            s.used.insert(
                name.to_owned(),
                (today.clone(), used.saturating_add(started)),
            );
        });
        if let Err(err) = counted {
            println!("autopilot: cannot save {name}'s budget: {err}");
        }
        let runner = self.clone();
        let (name, limit) = (name.to_owned(), goal.budget.max_runtime);
        tokio::spawn(async move {
            tokio::time::sleep(limit).await;
            let stopped = ids
                .iter()
                .filter(|id| runner.scheduler.cancel(**id))
                .count();
            if stopped > 0 {
                runner.pause(&name, "it ran longer than its max_runtime");
            }
        });
    }

    /// Pauses goal `name` and tells the owner why.
    fn pause(&self, name: &str, why: &str) {
        let saved = autopilot::update(&self.data, |s| s.paused.insert(name.to_owned()));
        let text = format!("Autopilot paused {name}: {why}.");
        println!("autopilot: {text}");
        if let Err(err) = saved {
            println!("autopilot: cannot save the pause: {err}");
        }
        let _ = self.notices.send(Notice {
            rule: "autopilot".to_owned(),
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_means_home() {
        let dirs = directories::BaseDirs::new().unwrap();
        let home = assist_actions::expand_home;
        assert_eq!(home("~/Downloads"), dirs.home_dir().join("Downloads"));
        assert_eq!(home("/tmp/x"), PathBuf::from("/tmp/x"));
    }
}
