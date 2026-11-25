//! Built-in actions (PLAN.md section 3.9). Each lives behind its own Cargo feature; removing one
//! never changes the core.

use std::sync::Arc;

use assist_core::action::Action;

#[cfg(feature = "apps")]
pub mod apps;
#[cfg(feature = "autopilot")]
pub mod autopilot;
#[cfg(feature = "facts")]
pub mod facts;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "mute")]
pub mod mute;
#[cfg(feature = "shell")]
mod process;
#[cfg(feature = "processes")]
pub mod processes;
#[cfg(feature = "record")]
pub mod record;
#[cfg(feature = "rename")]
pub mod rename;
#[cfg(all(feature = "screen", windows))]
pub mod screen;
#[cfg(feature = "scripts")]
pub mod scripts;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "shell")]
pub mod shell;
#[cfg(feature = "transcribe")]
pub mod transcribe;
#[cfg(feature = "transforms")]
pub mod transforms;

/// Every action compiled into this build.
// Each push is feature-gated, so a `vec![]` literal does not work; with no features the
// vector stays empty and `mut` is unused.
#[allow(clippy::vec_init_then_push, unused_mut)]
pub fn builtin() -> Vec<Arc<dyn Action>> {
    let mut actions: Vec<Arc<dyn Action>> = Vec::new();
    #[cfg(feature = "shell")]
    actions.push(Arc::new(shell::Shell));
    #[cfg(feature = "scripts")]
    actions.push(Arc::new(scripts::RunScript));
    #[cfg(feature = "apps")]
    actions.push(Arc::new(apps::OpenApp));
    #[cfg(feature = "search")]
    actions.push(Arc::new(search::Search));
    #[cfg(feature = "transforms")]
    actions.extend(transforms::all());
    #[cfg(feature = "rename")]
    actions.extend(rename::all());
    #[cfg(all(feature = "screen", windows))]
    actions.extend(screen::all());
    #[cfg(feature = "record")]
    actions.extend(record::all());
    #[cfg(feature = "transcribe")]
    actions.extend(transcribe::all());
    #[cfg(feature = "mute")]
    actions.extend(mute::all());
    #[cfg(feature = "memory")]
    actions.extend(memory::all());
    #[cfg(feature = "autopilot")]
    actions.extend(autopilot::all());
    actions
}

/// `~/x` as a path in the home folder; other paths unchanged.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    let path = path.trim();
    match (path.strip_prefix('~'), directories::BaseDirs::new()) {
        (Some(rest), Some(dirs)) => dirs.home_dir().join(rest.trim_start_matches(['/', '\\'])),
        _ => std::path::PathBuf::from(path),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use assist_core::action::{Action, ActionError, Chunk, JobCtx};
    use assist_core::config::Config;
    use assist_core::dialog::Plan;
    use assist_core::policy::Principal;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    /// A context for running `command` with `slots` in `cwd`.
    pub fn ctx(command: &str, slots: &[(&str, &str)], config: &str, cwd: PathBuf) -> JobCtx {
        JobCtx {
            job: 1,
            plan: Plan {
                command: command.to_owned(),
                slots: slots
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect::<BTreeMap<_, _>>(),
                typed: true,
            },
            principal: Principal::local("test"),
            config: Arc::new(Config::from_toml(config).unwrap()),
            cwd,
            cancel: CancellationToken::new(),
            data_dir: None,
        }
    }

    /// Runs `action` to the end and collects its output.
    pub async fn run(action: &dyn Action, ctx: JobCtx) -> (Result<(), ActionError>, Vec<Chunk>) {
        run_with_input(action, ctx, &[]).await
    }

    /// Runs `action` with `lines` as its pipe input and collects its output.
    pub async fn run_with_input(
        action: &dyn Action,
        ctx: JobCtx,
        lines: &[&str],
    ) -> (Result<(), ActionError>, Vec<Chunk>) {
        let (input_tx, input) = mpsc::channel(lines.len().max(1));
        for line in lines {
            let _ = input_tx.try_send(Chunk::Line((*line).to_owned()));
        }
        drop(input_tx);
        let (out, mut output) = mpsc::channel(256);
        let result = action.run(ctx, input, out).await;
        let mut chunks = Vec::new();
        while let Ok(chunk) = output.try_recv() {
            // Process ids are for the scheduler, never shown.
            if !matches!(chunk, Chunk::Process(_)) {
                chunks.push(chunk);
            }
        }
        (result, chunks)
    }
}
