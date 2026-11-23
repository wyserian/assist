//! Windows: PowerShell, and a Job Object so stopping a job ends every child process.

use process_wrap::tokio::{CommandWrap, JobObject};
use tokio::process::Command;

use super::on_path;

/// `pwsh` (PowerShell 7) if installed, else Windows PowerShell. No profile, never prompts.
pub(crate) fn shell_command(script: &str) -> Command {
    let program = if on_path("pwsh.exe") {
        "pwsh"
    } else {
        "powershell"
    };
    let mut command = Command::new(program);
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command
}

pub(super) fn contain(wrap: &mut CommandWrap) {
    wrap.wrap(JobObject);
}
