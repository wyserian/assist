//! `$ ...`: runs one non-interactive command in PowerShell or bash, streaming its output.
//!
//! `cd <dir>` is handled here so the working directory persists per conversation.
//! `cd` inside a compound command (`cd x && make`) affects only that command.

use std::path::{Component, Path, PathBuf};

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::dialog::SHELL;
use assist_core::policy::Capability;

use crate::process;

/// Runs shell commands.
pub struct Shell;

static MANIFEST: Manifest = Manifest {
    name: SHELL,
    capabilities: &[Capability::Shell],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::SOURCE,
};

impl Action for Shell {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let script = ctx.plan.slots.get("command").cloned().unwrap_or_default();
            if let Some(target) = cd_target(&script) {
                return change_dir(&ctx.cwd, target, &out).await;
            }
            process::run(process::shell_command(&script), &ctx.cwd, &out).await
        })
    }
}

/// The directory of a plain `cd <dir>` (empty for `cd` alone), or `None` for anything else.
fn cd_target(script: &str) -> Option<&str> {
    let script = script.trim();
    let target = if script == "cd" {
        ""
    } else {
        script.strip_prefix("cd ")?.trim()
    };
    let compound = target.contains(['&', '|', ';', '>', '<']);
    (!compound).then_some(target)
}

async fn change_dir(cwd: &Path, target: &str, out: &Output) -> Result<(), ActionError> {
    let target = target.trim_matches(['"', '\'']);
    let home = || {
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        std::env::var_os(var)
            .map(PathBuf::from)
            .ok_or_else(|| ActionError::new("no home directory"))
    };
    let path = if target.is_empty() || target == "~" {
        home()?
    } else if let Some(rest) = target.strip_prefix("~/") {
        home()?.join(rest)
    } else {
        clean(&cwd.join(target))
    };
    let is_dir = tokio::fs::metadata(&path)
        .await
        .is_ok_and(|meta| meta.is_dir());
    if !is_dir {
        return Err(ActionError::new(format!(
            "not a directory: {}",
            path.display()
        )));
    }
    let _ = out.send(Chunk::Info(path.display().to_string())).await;
    let _ = out.send(Chunk::Cwd(path)).await;
    Ok(())
}

/// Removes `.` and resolves `..` without touching the disk (keeps plain `C:\` paths on
/// Windows, unlike `canonicalize`).
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx, run};

    fn shell(command: &str, cwd: PathBuf) -> JobCtx {
        ctx(SHELL, &[("command", command)], "", cwd)
    }

    #[tokio::test]
    async fn streams_output() {
        let (result, chunks) = run(&Shell, shell("echo hello", std::env::temp_dir())).await;
        assert_eq!(result, Ok(()));
        assert_eq!(chunks, [Chunk::Line("hello".to_owned())]);
    }

    #[tokio::test]
    async fn reports_exit_code_and_stderr() {
        let script = if cfg!(windows) {
            "[Console]::Error.WriteLine('oops'); exit 3"
        } else {
            "echo oops 1>&2; exit 3"
        };
        let (result, chunks) = run(&Shell, shell(script, std::env::temp_dir())).await;
        assert_eq!(result.unwrap_err().exit, Some(3));
        assert_eq!(chunks, [Chunk::Error("oops".to_owned())]);
    }

    #[tokio::test]
    async fn dropping_the_job_stops_the_process() {
        let script = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let started = std::time::Instant::now();
        let job = run(&Shell, shell(script, std::env::temp_dir()));
        assert!(tokio::time::timeout(std::time::Duration::from_secs(3), job)
            .await
            .is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    fn alive(pid: u32) -> bool {
        let status = if cfg!(windows) {
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command"])
                .arg(format!(
                    "Get-Process -Id {pid} -ErrorAction Stop | Out-Null"
                ))
                .status()
        } else {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
        };
        status.is_ok_and(|s| s.success())
    }

    #[tokio::test]
    async fn stopping_kills_processes_the_command_started() {
        let pid_file =
            std::env::temp_dir().join(format!("assist-grandchild-{}", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let script = if cfg!(windows) {
            format!(
                "$p = Start-Process ping -ArgumentList '-n','60','127.0.0.1' -PassThru \
                 -WindowStyle Hidden; Set-Content -Path '{}' -Value $p.Id; Start-Sleep 60",
                pid_file.display()
            )
        } else {
            format!("sleep 60 & echo $! > '{}'; wait", pid_file.display())
        };
        let mut job = Box::pin(run(&Shell, shell(&script, std::env::temp_dir())));
        let pid = loop {
            tokio::select! {
                _ = &mut job => panic!("job ended before starting its child"),
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    let pid = std::fs::read_to_string(&pid_file).ok();
                    if let Some(pid) = pid.and_then(|p| p.trim().parse::<u32>().ok()) {
                        break pid;
                    }
                }
            }
        };
        assert!(alive(pid), "grandchild should be running");
        drop(job);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(!alive(pid), "grandchild {pid} survived the stop");
        let _ = std::fs::remove_file(&pid_file);
    }

    #[tokio::test]
    async fn cd_changes_the_working_directory() {
        let cwd = std::env::temp_dir().join("assist-cd-test");
        std::fs::create_dir_all(&cwd).unwrap();
        let (result, chunks) = run(&Shell, shell("cd ..", cwd.clone())).await;
        assert_eq!(result, Ok(()));
        let parent = clean(&cwd.join(".."));
        assert!(chunks.contains(&Chunk::Cwd(parent)));

        let (result, _) = run(&Shell, shell("cd does-not-exist", cwd.clone())).await;
        assert!(result.unwrap_err().message.starts_with("not a directory"));
        std::fs::remove_dir_all(&cwd).unwrap();
    }

    #[test]
    fn only_plain_cd_is_intercepted() {
        assert_eq!(cd_target("cd"), Some(""));
        assert_eq!(cd_target("cd  src "), Some("src"));
        assert_eq!(cd_target("cd .. && ls"), None);
        assert_eq!(cd_target("cdrom"), None);
        assert_eq!(cd_target("echo cd x"), None);
    }

    #[test]
    fn clean_resolves_dots() {
        let base = std::env::temp_dir();
        assert_eq!(
            clean(&base.join("a").join("..").join(".").join("b")),
            base.join("b")
        );
    }
}
