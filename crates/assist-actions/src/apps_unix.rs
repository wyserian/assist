//! Unix: starts the program directly, detached from our stdio.
//!
//! shortcut: the child is not waited on, so it stays a zombie entry after it exits until the
//! daemon exits; reap it from a background task when the daemon is long-running (Phase 1b).

use std::process::{Command, Stdio};

pub(super) fn launch(program: &str) -> std::io::Result<()> {
    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}
