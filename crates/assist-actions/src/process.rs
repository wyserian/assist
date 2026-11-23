//! Running child processes: output streamed line by line, the whole process tree killed when
//! the job stops (Job Object on Windows, process group on Unix).

use std::path::Path;
use std::process::Stdio;

use assist_core::action::{ActionError, Chunk, Output};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

#[cfg(unix)]
#[path = "process_unix.rs"]
mod os;
#[cfg(windows)]
#[path = "process_windows.rs"]
mod os;

pub(crate) use os::shell_command;

/// Kills the process tree when dropped: on stop, timeout, or when the job ends.
struct Running(Box<dyn ChildWrapper>);

impl Drop for Running {
    fn drop(&mut self) {
        // Already exited is fine.
        let _ = self.0.start_kill();
    }
}

/// Runs `command` in `cwd`, streams stdout as [`Chunk::Line`] and stderr as
/// [`Chunk::Error`], and fails with the exit code if it is not 0.
pub(crate) async fn run(mut command: Command, cwd: &Path, out: &Output) -> Result<(), ActionError> {
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrap = CommandWrap::from(command);
    os::contain(&mut wrap);
    wrap.wrap(KillOnDrop);
    let child = wrap
        .spawn()
        .map_err(|err| ActionError::new(format!("cannot start: {err}")))?;
    let mut running = Running(child);
    let stdout = running.0.stdout().take();
    let stderr = running.0.stderr().take();
    let (_, _, status) = tokio::join!(
        forward(stdout, out, Chunk::Line),
        forward(stderr, out, Chunk::Error),
        running.0.wait(),
    );
    let status = status.map_err(|err| ActionError::new(format!("cannot wait: {err}")))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(ActionError::exit(code)),
        None => Err(ActionError::new("stopped by a signal")),
    }
}

async fn forward<R: AsyncRead + Unpin>(reader: Option<R>, out: &Output, make: fn(String) -> Chunk) {
    let Some(reader) = reader else { return };
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                // Non-UTF-8 output (e.g. an OEM code page) is shown, not dropped.
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                if out.send(make(line)).await.is_err() {
                    return;
                }
            }
        }
    }
}
