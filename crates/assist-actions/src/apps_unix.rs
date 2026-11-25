//! Unix: starts the program directly, detached from our stdio. A small thread waits for it,
//! so it leaves no zombie process when it exits.

use std::process::{Command, Stdio};

/// shortcut: nothing is found on Linux; read `.desktop` files (their launch lines take
/// arguments, which `launch` does not) when a Linux desktop wants found apps.
pub(super) fn installed() -> Vec<(String, String)> {
    Vec::new()
}

/// shortcut: always launches on Linux; find the window through X11 or Wayland when a Linux
/// desktop wants "open" to switch to it.
pub(super) fn bring_to_front(_name: &str, _hint: &str) -> std::io::Result<bool> {
    Ok(false)
}

/// shortcut: closing apps is Windows only; send SIGTERM to the app's processes when a Linux
/// desktop wants it.
pub(super) fn close(name: &str, _hint: &str) -> std::io::Result<usize> {
    Err(std::io::Error::other(format!(
        "closing {name} is not supported on Linux yet"
    )))
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
