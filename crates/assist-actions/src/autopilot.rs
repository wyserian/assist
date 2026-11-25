//! "stop autopilot" and "start autopilot" (PLAN.md section 3.18). Stopping is a safety
//! switch anyone may use; starting again needs the owner (`config.write`: local only).

use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::autopilot;
use assist_core::policy::Capability;

/// Stop and start.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Switch(false)), Arc::new(Switch(true))]
}

/// Turns every goal off (`false`) or on again.
pub struct Switch(bool);

static STOP: Manifest = Manifest {
    name: "stop_autopilot",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

static START: Manifest = Manifest {
    name: "start_autopilot",
    capabilities: &[Capability::ConfigWrite],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

impl Action for Switch {
    fn manifest(&self) -> &Manifest {
        if self.0 {
            &START
        } else {
            &STOP
        }
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let on = self.0;
        Box::pin(async move {
            let data = ctx
                .data_dir
                .clone()
                .ok_or_else(|| ActionError::new("autopilot needs the data directory"))?;
            tokio::task::spawn_blocking(move || autopilot::update(&data, |s| s.off = !on))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .map_err(|err| ActionError::new(format!("cannot save: {err}")))?;
            let text = if on {
                "Autopilot is on."
            } else {
                "Autopilot is off."
            };
            let _ = out.send(Chunk::Info(text.to_owned())).await;
            Ok(())
        })
    }
}
