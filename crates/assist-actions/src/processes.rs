//! Job processes for the scheduler (PLAN.md section 3.8): suspend, resume and measure a
//! job's processes with all their descendants. sysinfo walks the process tree; Linux
//! suspends with SIGSTOP and SIGCONT, Windows with the audited calls in
//! `processes_win32.rs`.

use std::collections::{BTreeSet, HashMap};

use assist_core::scheduler::{Processes, Usage};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[cfg(windows)]
#[path = "processes_win32.rs"]
mod win32;

/// The OS side of job control.
#[derive(Debug, Default)]
pub struct OsProcesses;

impl Processes for OsProcesses {
    fn suspend(&self, roots: &[u32]) -> Result<(), String> {
        for_each(roots, Op::Suspend)
    }

    fn resume(&self, roots: &[u32]) -> Result<(), String> {
        for_each(roots, Op::Resume)
    }

    fn usage(&self, groups: &[Vec<u32>]) -> Vec<Option<Usage>> {
        let Ok(mut system) = System::new() else {
            return vec![None; groups.len()];
        };
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        let children = children(&system);
        groups
            .iter()
            .map(|roots| {
                let tree = tree(&system, &children, roots);
                if tree.is_empty() {
                    return None;
                }
                let (cpu, memory) = tree
                    .iter()
                    .filter_map(|pid| system.process(*pid))
                    .fold((0.0f32, 0u64), |(cpu, mem), p| {
                        (cpu + p.cpu_usage(), mem + p.memory())
                    });
                Some(Usage {
                    cpu: (cpu / cores).round() as u32,
                    memory,
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
enum Op {
    Suspend,
    Resume,
}

/// Applies `op` to every process in the trees under `roots`, children first on suspend so a
/// parent cannot start new children in between, parents first on resume.
fn for_each(roots: &[u32], op: Op) -> Result<(), String> {
    let mut system = System::new().map_err(|err| format!("cannot list processes: {err}"))?;
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let children = children(&system);
    let mut pids = tree(&system, &children, roots);
    if pids.is_empty() {
        return Err("its processes have ended".to_owned());
    }
    if matches!(op, Op::Suspend) {
        pids.reverse();
    }
    for pid in pids {
        apply(&system, pid, op)?;
    }
    Ok(())
}

#[cfg(windows)]
fn apply(_system: &System, pid: Pid, op: Op) -> Result<(), String> {
    match op {
        Op::Suspend => win32::suspend(pid.as_u32()),
        Op::Resume => win32::resume(pid.as_u32()),
    }
}

#[cfg(not(windows))]
fn apply(system: &System, pid: Pid, op: Op) -> Result<(), String> {
    let signal = match op {
        Op::Suspend => sysinfo::Signal::Stop,
        Op::Resume => sysinfo::Signal::Continue,
    };
    match system.process(pid).and_then(|p| p.kill_with(signal)) {
        Some(true) | None => Ok(()),
        Some(false) => Err(format!("cannot signal process {pid}")),
    }
}

fn children(system: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    children
}

/// The roots that still run and all their descendants, parents before children.
fn tree(system: &System, children: &HashMap<Pid, Vec<Pid>>, roots: &[u32]) -> Vec<Pid> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    let mut next: Vec<Pid> = roots
        .iter()
        .map(|r| Pid::from_u32(*r))
        .filter(|pid| system.process(*pid).is_some())
        .collect();
    while let Some(pid) = next.pop() {
        if !seen.insert(pid) {
            continue;
        }
        order.push(pid);
        if let Some(kids) = children.get(&pid) {
            next.extend(kids.iter().copied());
        }
    }
    order
}
