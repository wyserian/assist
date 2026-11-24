//! How jobs and their output are printed, shared by `assist shell` and the daemon clients.

use assist_proto::{JobState, JobSummary};

/// `text` with control characters replaced, so job output cannot move the cursor, change the
/// terminal title or hide text.
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
        .collect()
}

/// One row of a job listing.
pub fn job_row(job: &JobSummary) -> String {
    format!(
        "#{:<4} {:<11} {:<12} {}",
        job.id,
        job.state.as_str(),
        job.lane.as_str(),
        printable(&job.title)
    )
}

/// A line of job output.
pub fn output_line(id: u64, error: bool, text: &str) -> String {
    let mark = if error { "! " } else { "" };
    format!("[#{id}] {mark}{}", printable(text))
}

/// The line for a job that ended; `None` while it is queued or running.
pub fn ended_line(id: u64, state: JobState, error: Option<&str>) -> Option<String> {
    let text = match state {
        JobState::Queued | JobState::Running => return None,
        JobState::Failed => match error {
            Some(reason) => format!("failed: {}", printable(reason)),
            None => "failed".to_owned(),
        },
        JobState::Cancelled => "stopped".to_owned(),
        JobState::Done | JobState::TimedOut | JobState::Interrupted => state.as_str().to_owned(),
    };
    Some(format!("[#{id}] {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_not_printed() {
        assert_eq!(printable("ok\x1b]0;pwned\x07\tend"), "ok?]0;pwned?\tend");
    }

    #[test]
    fn ended_lines() {
        assert_eq!(ended_line(1, JobState::Running, None), None);
        assert_eq!(
            ended_line(2, JobState::Failed, Some("exited with code 3")).as_deref(),
            Some("[#2] failed: exited with code 3")
        );
        assert_eq!(
            ended_line(3, JobState::Cancelled, None).as_deref(),
            Some("[#3] stopped")
        );
        assert_eq!(output_line(4, true, "oops"), "[#4] ! oops");
    }
}
