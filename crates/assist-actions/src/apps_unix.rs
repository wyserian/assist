//! Unix: starts the program directly, detached from our stdio. A small thread waits for it,
//! so it leaves no zombie process when it exits.

use std::process::{Command, Stdio};

pub(super) fn launch(program: &str) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("assist-app-reaper".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}
