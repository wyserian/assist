//! Long-term memory commands (PLAN.md section 3.17), over `assist_core::memory`:
//!
//! - `remember Sam's number is 1234`: previews the sentence (refusing passwords, codes, keys
//!   and card numbers), then saves it.
//! - `what's Sam's number`: answered at once, no job.
//! - `forget Sam's number` / `forget everything`: previews every record it deletes.

use std::path::Path;
use std::sync::Arc;

use assist_core::action::{
    matches_preview, Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane,
    Manifest, Output, PipeShape,
};
use assist_core::audit::now_ms;
use assist_core::config::Config;
use assist_core::dialog::Plan;
use assist_core::memory::{self, Record};
use assist_core::policy::Capability;

const fn shape(arg: &'static str) -> PipeShape {
    PipeShape {
        input: DataKind::Empty,
        output: DataKind::Empty,
        arg: Some(arg),
        untrusted: false,
    }
}

/// Remember, recall and forget.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Remember), Arc::new(Recall), Arc::new(Forget)]
}

/// `remember <fact>`.
pub struct Remember;

static REMEMBER: Manifest = Manifest {
    name: "remember",
    capabilities: &[Capability::MemoryWrite],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: shape("text"),
};

/// `what's <subject>`: answered at once.
pub struct Recall;

static RECALL: Manifest = Manifest {
    name: "recall",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: shape("subject"),
};

/// `forget <subject>` or `forget everything`.
pub struct Forget;

static FORGET: Manifest = Manifest {
    name: "forget",
    capabilities: &[Capability::Delete],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: shape("subject"),
};

fn slot<'a>(plan: &'a Plan, name: &str) -> &'a str {
    plan.slots.get(name).map_or("", String::as_str)
}

fn data(dir: Option<&Path>) -> Result<&Path, String> {
    dir.ok_or_else(|| "memory needs the data directory".to_owned())
}

impl Action for Remember {
    fn manifest(&self) -> &Manifest {
        &REMEMBER
    }

    fn preview(
        &self,
        plan: &Plan,
        _config: &Config,
        _data_dir: Option<&Path>,
    ) -> Option<Result<Vec<String>, String>> {
        let sentence = memory::check(slot(plan, "text")).map(|(subject, fact)| {
            let record = Record {
                id: 0,
                subject,
                fact,
                source: String::new(),
                created: 0,
                last_used: 0,
            };
            format!("Remember: {}", record.sentence())
        });
        Some(sentence.map(|line| vec![line]))
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let dir = ctx.data_dir.clone();
            let text = slot(&ctx.plan, "text").to_owned();
            let source = ctx.principal.source.clone();
            let record =
                blocking(move || memory::add(data(dir.as_deref())?, &text, &source, now_ms()))
                    .await?;
            let _ = out
                .send(Chunk::Info(format!("Remembered: {}", record.sentence())))
                .await;
            Ok(())
        })
    }
}

impl Action for Recall {
    fn manifest(&self) -> &Manifest {
        &RECALL
    }

    fn answer(
        &self,
        plan: &Plan,
        _config: &Config,
        data_dir: Option<&Path>,
    ) -> Option<Result<String, String>> {
        Some(recall(plan, data_dir))
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        // Runs only when the answer needed a confirmation first (untrusted input).
        Box::pin(async move {
            let plan = ctx.plan.clone();
            let dir = ctx.data_dir.clone();
            let line = tokio::task::spawn_blocking(move || recall(&plan, dir.as_deref()))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .unwrap_or_else(|reason| reason);
            let _ = out.send(Chunk::Line(line)).await;
            Ok(())
        })
    }
}

fn recall(plan: &Plan, dir: Option<&Path>) -> Result<String, String> {
    let subject = slot(plan, "subject");
    match memory::recall(data(dir)?, subject, now_ms()) {
        Some(record) if record.fact.is_empty() => Ok(format!("You told me: {}", record.sentence())),
        Some(record) => Ok(record.sentence()),
        None => Err(format!("I don't remember anything about {subject}.")),
    }
}

impl Action for Forget {
    fn manifest(&self) -> &Manifest {
        &FORGET
    }

    fn preview(
        &self,
        plan: &Plan,
        _config: &Config,
        data_dir: Option<&Path>,
    ) -> Option<Result<Vec<String>, String>> {
        let found = data(data_dir).map(|dir| lines(&memory::matching(dir, slot(plan, "subject"))));
        Some(found)
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let plan = ctx.plan.clone();
            let dir = ctx.data_dir.clone();
            let gone = blocking(move || {
                let dir = data(dir.as_deref())?;
                let records = memory::matching(dir, slot(&plan, "subject"));
                if !matches_preview(&plan, &lines(&records)) {
                    return Err("memory changed since you confirmed; ask again".to_owned());
                }
                let ids: Vec<u64> = records.iter().map(|r| r.id).collect();
                memory::forget(dir, &ids)
            })
            .await?;
            let _ = out
                .send(Chunk::Info(format!(
                    "Forgot {gone} {}.",
                    if gone == 1 { "thing" } else { "things" }
                )))
                .await;
            Ok(())
        })
    }
}

/// `#id sentence` per record.
fn lines(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .map(|r| format!("#{} {}", r.id, r.sentence()))
        .collect()
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, ActionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ActionError::new(err.to_string()))?
        .map_err(ActionError::new)
}
