//! "open firefox": launches an app from `[apps]`, detached from the job.
//!
//! An app is a program name or path, or a table with one per OS:
//! `firefox = { windows = "firefox.exe", linux = "firefox" }`.

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::policy::Capability;

#[cfg(unix)]
#[path = "apps_unix.rs"]
mod os;
#[cfg(windows)]
#[path = "apps_windows.rs"]
mod os;

/// Opens configured apps.
pub struct OpenApp;

static MANIFEST: Manifest = Manifest {
    name: "open_app",
    capabilities: &[Capability::Apps],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

impl Action for OpenApp {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let name = ctx.plan.slots.get("app").cloned().unwrap_or_default();
            let program = ctx
                .config
                .apps
                .get(&name)
                .and_then(program_for_this_os)
                .ok_or_else(|| ActionError::new(format!("no program for {name:?} on this OS")))?;
            tokio::task::spawn_blocking(move || os::launch(&program))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .map_err(|err| ActionError::new(format!("cannot open {name}: {err}")))?;
            let _ = out.send(Chunk::Info(format!("Opened {name}"))).await;
            Ok(())
        })
    }
}

fn program_for_this_os(value: &toml::Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get(std::env::consts::OS)?.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_program_for_this_os() {
        let value: toml::Value =
            toml::from_str("windows = \"firefox.exe\"\nlinux = \"firefox\"\nmacos = \"firefox\"")
                .unwrap();
        let expected = if cfg!(windows) {
            "firefox.exe"
        } else {
            "firefox"
        };
        assert_eq!(program_for_this_os(&value).as_deref(), Some(expected));
        assert_eq!(
            program_for_this_os(&toml::Value::String("code".to_owned())).as_deref(),
            Some("code")
        );
        let android_only: toml::Value = toml::from_str("android = \"org.x\"").unwrap();
        assert_eq!(program_for_this_os(&android_only), None);
    }
}
