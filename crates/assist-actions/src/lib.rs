//! Built-in actions (PLAN.md section 3.9). Each lives behind its own Cargo feature; removing one
//! never changes the core.

use std::sync::Arc;

use assist_core::action::Action;

#[cfg(feature = "apps")]
pub mod apps;
#[cfg(feature = "shell")]
mod process;
#[cfg(feature = "scripts")]
pub mod scripts;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "shell")]
pub mod shell;

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
    actions
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
        }
    }

    /// Runs `action` to the end and collects its output.
    pub async fn run(action: &dyn Action, ctx: JobCtx) -> (Result<(), ActionError>, Vec<Chunk>) {
        let (_input_tx, input) = mpsc::channel(1);
        let (out, mut output) = mpsc::channel(256);
        let result = action.run(ctx, input, out).await;
        let mut chunks = Vec::new();
        while let Ok(chunk) = output.try_recv() {
            chunks.push(chunk);
        }
        (result, chunks)
    }
}
