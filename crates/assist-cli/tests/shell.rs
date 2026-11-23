//! End-to-end: drives `assist shell` through stdin, like a user would.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn shell_runs_commands_and_audits_them() {
    let dir = std::env::temp_dir().join(format!("assist-cli-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.toml");
    std::fs::write(&config, "[scripts]\nhello = \"echo built\"\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_assist"))
        .args(["shell", "--config"])
        .arg(&config)
        .arg("--data-dir")
        .arg(dir.join("data"))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = "$ echo hello\nrun hello\nmake me a sandwich\nno\njobs\nstop 99\n";
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");

    for expected in [
        "[#1] $ echo hello",
        "[#1] hello",
        "[#2] run_script: script = hello",
        "[#2] built",
        "Run \"make me a sandwich\" in the shell? (yes/no)",
        "Okay, not doing that.",
        "No running job #99.",
        "[#1] done",
        "[#2] done",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }

    let audit = std::fs::read_dir(dir.join("data").join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // submitted, running and done for each of the two jobs.
    assert_eq!(assist_core::audit::verify(&audit).unwrap(), 6);
    std::fs::remove_dir_all(&dir).unwrap();
}
