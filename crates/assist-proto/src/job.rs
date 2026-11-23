//! Job states and lanes, shared by the scheduler and every client.

use serde::{Deserialize, Serialize};

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Was queued or running when the daemon stopped.
    Interrupted,
}

impl JobState {
    /// Whether the job has ended.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    /// Lowercase name, as on the wire and in the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Scheduler lane (PLAN.md section 3.8). A background job never delays an interactive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Quick, user-facing jobs.
    Interactive,
    /// Long jobs: builds, scans, scrapes.
    Background,
}

impl Lane {
    /// The config and wire name, e.g. `background`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_serde() {
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Done,
            JobState::Failed,
            JobState::Cancelled,
            JobState::TimedOut,
            JobState::Interrupted,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
        }
        for lane in [Lane::Interactive, Lane::Background] {
            let json = serde_json::to_string(&lane).unwrap();
            assert_eq!(json, format!("\"{}\"", lane.as_str()));
        }
    }
}
