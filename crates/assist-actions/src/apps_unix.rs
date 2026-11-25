//! Unix: starts the program directly, detached from our stdio. A small thread waits for it,
//! so it leaves no zombie process when it exits.

use std::process::{Command, Stdio};

/// shortcut: nothing is found on Linux; read `.desktop` files (their launch lines take
/// arguments, which `launch` does not) when a Linux desktop wants found apps.
pub(super) fn installed() -> Vec<(String, String)> {
    Vec::new()
}

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
