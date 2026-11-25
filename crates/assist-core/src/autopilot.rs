//! Autopilot goals, rule-based (PLAN.md section 3.18): standing goals that run a saved pipe
//! on a trigger with nobody watching, limited to the goal's allowed commands and budgets.
//! The run state the owner changes (off, paused goals) and today's job counts live in
//! `<data>/autopilot.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// Never done with nobody watching, whatever a goal allows (PLAN.md section 3.18).
pub const NEVER_AUTONOMOUS: [crate::policy::Capability; 8] = {
    use crate::policy::Capability as C;
    [
        C::Delete,
        C::Send,
        C::NetScan,
        C::SecretsWrite,
        C::ConfigWrite,
        C::Admin,
        C::Record,
        C::MemoryWrite,
    ]
};

/// What the owner switched, and what goals used today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Every goal stopped ("stop autopilot").
    #[serde(default)]
    pub off: bool,
    /// Goals paused by the owner or by a budget.
    #[serde(default)]
    pub paused: BTreeSet<String>,
    /// Goal name to (local date, jobs started that day).
    #[serde(default)]
    pub used: BTreeMap<String, (String, u32)>,
}

impl State {
    /// Jobs goal `name` started on `today` (`YYYY-MM-DD`).
    pub fn used_on(&self, name: &str, today: &str) -> u32 {
        match self.used.get(name) {
            Some((day, count)) if day == today => *count,
            _ => 0,
        }
    }
}

/// Serializes changes within the daemon.
static LOCK: Mutex<()> = Mutex::new(());

fn path(data: &Path) -> PathBuf {
    data.join("autopilot.json")
}

/// The saved state; default when there is none.
pub fn load(data: &Path) -> State {
    std::fs::read(path(data))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Changes the saved state under a lock and saves it, user-only.
pub fn update<T>(data: &Path, change: impl FnOnce(&mut State) -> T) -> std::io::Result<T> {
    let _lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut state = load(data);
    let out = change(&mut state);
    let json = serde_json::to_vec_pretty(&state).map_err(std::io::Error::from)?;
    crate::private_fs::write_atomic(&path(data), &json)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_counts_per_day() {
        let data = std::env::temp_dir().join(format!("assist-autopilot-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        update(&data, |s| {
            s.paused.insert("tidy".to_owned());
            s.used
                .insert("tidy".to_owned(), ("2026-11-27".to_owned(), 3));
        })
        .unwrap();
        let state = load(&data);
        assert!(state.paused.contains("tidy"));
        assert_eq!(state.used_on("tidy", "2026-11-27"), 3);
        assert_eq!(state.used_on("tidy", "2026-11-28"), 0);
        std::fs::remove_dir_all(&data).unwrap();
    }
}
