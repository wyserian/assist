//! "shut down": stops the daemon or the shell that runs it. A safety switch anyone may use,
//! like "stop autopilot" (capability `read`); start it again with `assist serve`.

use std::time::Duration;

use assist_core::action::{
    Action, ActionError, BoxFuture, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::policy::Capability;
use tokio_util::sync::CancellationToken;

/// Time for the reply ("Shutting down.") to be spoken before the daemon goes away.
const GRACE: Duration = Duration::from_secs(2);

/// Cancels `stop`, which the daemon and the shell wait on.
pub struct Shutdown {
    /// Cancelled to stop.
    pub stop: CancellationToken,
}

static MANIFEST: Manifest = Manifest {
    name: "shutdown",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

impl Action for Shutdown {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, _ctx: JobCtx, _input: Input, _out: Output) -> BoxFuture<Result<(), ActionError>> {
        let stop = self.stop.clone();
        Box::pin(async move {
            tokio::time::sleep(GRACE).await;
            stop.cancel();
            Ok(())
        })
    }
}
