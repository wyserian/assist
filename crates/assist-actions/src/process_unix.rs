//! Unix: `bash -c` (or `sh -c`), and a process group so stopping a job ends every child.

use process_wrap::tokio::{CommandWrap, ProcessGroup};
use tokio::process::Command;

use super::on_path;

/// `bash` if installed, else `sh`.
pub(crate) fn shell_command(script: &str) -> Command {
    let program = if on_path("bash") { "bash" } else { "sh" };
    let mut command = Command::new(program);
    command.args(["-c", script]);
    command
}

pub(super) fn contain(wrap: &mut CommandWrap) {
    wrap.wrap(ProcessGroup::leader());
}
