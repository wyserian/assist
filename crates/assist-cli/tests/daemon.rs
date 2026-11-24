//! End-to-end: `assist serve` in the background, driven by the client commands.

// Test-only file: helpers fail the test on any error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assist(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_assist"));
    command
        .arg("--config")
        .arg(dir.join("config.toml"))
        .arg("--data-dir")
        .arg(dir.join("data"))
        .args(args)
        .current_dir(dir);
    command
}

fn start() -> Daemon {
    let dir = std::env::temp_dir().join(format!("assist-cli-daemon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[server]\nbind = \"127.0.0.1:0\"\n",
    )
    .unwrap();
    let child = assist(&dir, &["serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let addr = dir.join("data").join("daemon.addr");
    let started = Instant::now();
    while !addr.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "daemon did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { child, dir }
}

fn run(daemon: &Daemon, args: &[&str]) -> (Output, String) {
    let output = assist(&daemon.dir, args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output, stdout)
}

#[test]
fn client_commands_talk_to_the_daemon() {
    let daemon = start();

    let (output, text) = run(&daemon, &["run", "$ echo hi"]);
    assert!(output.status.success(), "{text}");
    for expected in ["[#1] $ echo hi", "[#1] hi", "[#1] done"] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }

    let (_, text) = run(&daemon, &["jobs"]);
    assert!(text.contains("#1    done"), "{text}");

    let (_, text) = run(&daemon, &["log", "1"]);
    assert!(text.contains("hi"), "{text}");

    let (_, text) = run(&daemon, &["stop", "99"]);
    assert!(text.contains("No running job #99."), "{text}");

    let (_, text) = run(&daemon, &["explain", "$ ls"]);
    assert!(text.contains("decision: allow"), "{text}");

    let (output, text) = run(&daemon, &["run", "$ exit 3"]);
    assert!(!output.status.success(), "a failed job fails the command");
    assert!(text.contains("failed: exited with code 3"), "{text}");

    let (output, text) = run(&daemon, &["run", "make", "me", "a", "sandwich"]);
    assert!(!output.status.success());
    assert!(text.contains("run this in a terminal"), "{text}");
}

#[test]
fn clients_say_when_no_daemon_runs() {
    let dir = std::env::temp_dir().join(format!("assist-cli-nodaemon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let output = assist(&dir, &["jobs"]).output().unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("No daemon running"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
