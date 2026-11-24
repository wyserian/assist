//! "run build": runs a named script from `[scripts]` in the shell.
//!
//! A script is a command string, or a table with `run` or a per-OS command and an optional
//! `lane`:
//!
//! ```toml
//! [scripts]
//! backup = { windows = "C:/scripts/backup.ps1", linux = "~/scripts/backup.sh" }
//! build  = { run = "cargo build", lane = "background" }
//! ```
//!
//! The command is passed to the shell as written, so quote paths that contain spaces.

use assist_core::action::{
    Action, ActionError, BoxFuture, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::config::Config;
use assist_core::dialog::Plan;
use assist_core::policy::Capability;

use crate::process;

/// Runs named scripts.
pub struct RunScript;

static MANIFEST: Manifest = Manifest {
    name: "run_script",
    capabilities: &[Capability::Shell],
    owner_defined: true,
    lane: Lane::Interactive,
    pipe: PipeShape::SOURCE,
};

impl Action for RunScript {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn lane(&self, plan: &Plan, config: &Config) -> Lane {
        let background = script(plan, config)
            .and_then(|value| value.get("lane"))
            .and_then(toml::Value::as_str)
            == Some("background");
        if background {
            Lane::Background
        } else {
            MANIFEST.lane
        }
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let name = ctx.plan.slots.get("script").cloned().unwrap_or_default();
            let command = script(&ctx.plan, &ctx.config)
                .and_then(command_for_this_os)
                .ok_or_else(|| {
                    ActionError::new(format!("no command for script {name:?} on this OS"))
                })?;
            process::run(process::shell_command(&command), &ctx.cwd, &out).await
        })
    }
}

fn script<'a>(plan: &Plan, config: &'a Config) -> Option<&'a toml::Value> {
    config.scripts.get(plan.slots.get("script")?)
}

fn command_for_this_os(value: &toml::Value) -> Option<String> {
    if let Some(command) = value.as_str() {
        return Some(command.to_owned());
    }
    value
        .get("run")
        .or_else(|| value.get(std::env::consts::OS))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx, run};
    use assist_core::action::Chunk;

    const CONFIG: &str = r#"
[scripts]
hello = "echo built"
build = { run = "cargo build", lane = "background" }
backup = { windows = "backup.ps1", linux = "backup.sh", macos = "backup.sh" }
"#;

    #[test]
    fn picks_the_command_and_lane() {
        let config = Config::from_toml(CONFIG).unwrap();
        let plan = |name: &str| ctx("run_script", &[("script", name)], CONFIG, ".".into()).plan;
        let command = |name: &str| script(&plan(name), &config).and_then(command_for_this_os);
        assert_eq!(command("hello").as_deref(), Some("echo built"));
        assert_eq!(command("build").as_deref(), Some("cargo build"));
        let backup = if cfg!(windows) {
            "backup.ps1"
        } else {
            "backup.sh"
        };
        assert_eq!(command("backup").as_deref(), Some(backup));
        assert_eq!(RunScript.lane(&plan("build"), &config), Lane::Background);
        assert_eq!(RunScript.lane(&plan("hello"), &config), Lane::Interactive);
    }

    #[tokio::test]
    async fn runs_the_script() {
        let job = ctx(
            "run_script",
            &[("script", "hello")],
            CONFIG,
            std::env::temp_dir(),
        );
        let (result, chunks) = run(&RunScript, job).await;
        assert_eq!(result, Ok(()));
        assert_eq!(chunks, [Chunk::Line("built".to_owned())]);
    }

    #[tokio::test]
    async fn unknown_script_fails() {
        let job = ctx(
            "run_script",
            &[("script", "nope")],
            CONFIG,
            std::env::temp_dir(),
        );
        let (result, _) = run(&RunScript, job).await;
        assert!(result.is_err());
    }
}
