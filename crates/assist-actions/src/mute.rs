//! "stop telling me about build-failed" and "tell me about build-failed again": mutes and
//! unmutes a proactive rule (PLAN.md section 3.17). The names live in the data directory,
//! where the daemon's proactive engine reads them.

use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;
use assist_core::proactive::{load_muted, save_muted};

const SHAPE: PipeShape = PipeShape {
    input: DataKind::Empty,
    output: DataKind::Empty,
    arg: Some("rule"),
    untrusted: false,
};

/// Mute and unmute.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Mute(true)), Arc::new(Mute(false))]
}

/// Mutes (`true`) or unmutes a rule.
pub struct Mute(bool);

// Changes only what the assistant tells you, so it needs no more than reading.
static MUTE: Manifest = Manifest {
    name: "mute",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: SHAPE,
};

static UNMUTE: Manifest = Manifest {
    name: "unmute",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: SHAPE,
};

impl Action for Mute {
    fn manifest(&self) -> &Manifest {
        if self.0 {
            &MUTE
        } else {
            &UNMUTE
        }
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let mute = self.0;
        Box::pin(async move {
            let rule = ctx
                .plan
                .slots
                .get("rule")
                .map(|r| r.trim().to_lowercase())
                .unwrap_or_default();
            let known: Vec<&str> = ctx
                .config
                .proactive
                .rules
                .iter()
                .map(|r| r.name.as_str())
                .collect();
            if !known.contains(&rule.as_str()) {
                return Err(ActionError::new(format!(
                    "no rule named {rule:?}; rules: {}",
                    if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    }
                )));
            }
            let data = ctx
                .data_dir
                .clone()
                .ok_or_else(|| ActionError::new("muting needs the data directory"))?;
            let message = tokio::task::spawn_blocking(move || {
                let mut muted = load_muted(&data);
                if mute {
                    muted.insert(rule.clone());
                } else {
                    muted.remove(&rule);
                }
                save_muted(&data, &muted).map(|()| rule)
            })
            .await
            .map_err(|err| ActionError::new(err.to_string()))?
            .map_err(|err| ActionError::new(format!("cannot save the muted rules: {err}")))?;
            let text = if mute {
                format!("Muted {message}.")
            } else {
                format!("Unmuted {message}.")
            };
            let _ = out.send(Chunk::Info(text)).await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx, run};

    #[tokio::test]
    async fn mutes_and_unmutes_known_rules_only() {
        let data = std::env::temp_dir().join(format!("assist-mute-{}", std::process::id()));
        std::fs::create_dir_all(&data).unwrap();
        let config =
            "[[proactive.rules]]\nname = \"build-failed\"\non = \"job.failed\"\nsay = \"x\"\n";
        let mut job = ctx("mute", &[("rule", "Build-Failed")], config, data.clone());
        job.data_dir = Some(data.clone());
        run(&Mute(true), job.clone()).await.0.unwrap();
        assert!(load_muted(&data).contains("build-failed"));
        job.plan.command = "unmute".to_owned();
        run(&Mute(false), job.clone()).await.0.unwrap();
        assert!(load_muted(&data).is_empty());
        job.plan.slots.insert("rule".to_owned(), "nope".to_owned());
        assert!(run(&Mute(true), job)
            .await
            .0
            .unwrap_err()
            .message
            .contains("no rule"));
        std::fs::remove_dir_all(&data).unwrap();
    }
}
