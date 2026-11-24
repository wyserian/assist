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

/// Longer output lines are split, so a process that never prints a newline cannot grow memory.
const MAX_LINE: usize = 16 * 1024;

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

/// Whether `program` (with extension on Windows) is in a `PATH` directory.
pub(crate) fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Sends each line of `reader` (at most [`MAX_LINE`] bytes) until it ends or nobody listens.
async fn forward<R: AsyncRead + Unpin>(reader: Option<R>, out: &Output, make: fn(String) -> Chunk) {
    let Some(reader) = reader else { return };
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(bytes) => bytes,
        };
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline
            .map_or(available.len(), |at| at + 1)
            .min(MAX_LINE - line.len());
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        let complete = line.last() == Some(&b'\n') || line.len() >= MAX_LINE;
        if complete && !send(out, make, &mut line).await {
            return;
        }
    }
    if !line.is_empty() {
        send(out, make, &mut line).await;
    }
}

/// Sends `line` without its line ending and clears it; false once nobody listens.
async fn send(out: &Output, make: fn(String) -> Chunk, line: &mut Vec<u8>) -> bool {
    // Non-UTF-8 output (e.g. an OEM code page) is shown, not dropped.
    let text = String::from_utf8_lossy(line)
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    line.clear();
    out.send(make(text)).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    async fn lines(input: &[u8]) -> Vec<Chunk> {
        let (out, mut rx) = mpsc::channel(64);
        forward(Some(input), &out, Chunk::Line).await;
        drop(out);
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk);
        }
        chunks
    }

    #[tokio::test]
    async fn splits_lines_and_caps_their_length() {
        let line = |s: &str| Chunk::Line(s.to_owned());
        assert_eq!(lines(b"a\r\nb\nc").await, [line("a"), line("b"), line("c")]);
        let long = vec![b'x'; MAX_LINE + 10];
        let chunks = lines(&long).await;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1], line("xxxxxxxxxx"));
    }
}
