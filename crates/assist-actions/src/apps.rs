//! "open firefox": launches an app from `[apps]`, detached from the job.
//!
//! An app is a program name or path, or a table with one per OS:
//! `firefox = { windows = "firefox.exe", linux = "firefox" }`. [`installed`] finds the apps
//! this device has, which join `[apps]` at start (config.toml entries win).

use std::sync::Arc;

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

/// The apps installed on this device, as (name, how to launch it). Blocking: it asks the
/// OS, which takes about a second on Windows.
pub fn installed() -> Vec<(String, String)> {
    os::installed()
}

/// Open and close.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(OpenApp), Arc::new(CloseApp)]
}

/// Opens configured apps, or switches to one that is already open.
pub struct OpenApp;

/// Closes an open app the way its close button does, so unsaved work still asks.
pub struct CloseApp;

static MANIFEST: Manifest = Manifest {
    name: "open_app",
    capabilities: &[Capability::Apps],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

static CLOSE: Manifest = Manifest {
    name: "close_app",
    ..MANIFEST
};

/// The app's name from the plan and how this OS launches it.
fn resolve(ctx: &JobCtx) -> Result<(String, String), ActionError> {
    let name = ctx.plan.slots.get("app").cloned().unwrap_or_default();
    let program = ctx
        .config
        .apps
        .get(&name)
        .and_then(program_for_this_os)
        .ok_or_else(|| ActionError::new(format!("no program for {name:?} on this OS")))?;
    Ok((name, program))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, ActionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ActionError::new(err.to_string()))?
        .map_err(|err| ActionError::new(err.to_string()))
}

impl Action for OpenApp {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let (name, program) = resolve(&ctx)?;
            let (app, hint) = (name.clone(), process_hint(&program));
            if blocking(move || os::bring_to_front(&app, &hint)).await? {
                let _ = out.send(Chunk::Info(format!("Switched to {name}"))).await;
                return Ok(());
            }
            blocking(move || os::launch(&program))
                .await
                .map_err(|err| ActionError::new(format!("cannot open {name}: {err}")))?;
            let _ = out.send(Chunk::Info(format!("Opened {name}"))).await;
            Ok(())
        })
    }
}

impl Action for CloseApp {
    fn manifest(&self) -> &Manifest {
        &CLOSE
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let (name, program) = resolve(&ctx)?;
            let (app, hint) = (name.clone(), process_hint(&program));
            let closed = blocking(move || os::close(&app, &hint)).await?;
            if closed == 0 {
                return Err(ActionError::new(format!("{name} is not open")));
            }
            let _ = out.send(Chunk::Info(format!("Closed {name}"))).await;
            Ok(())
        })
    }
}

/// The process name a launch value points at, lower case, for finding the app's windows:
/// `firefox.exe` and `C:/x/Firefox.exe` give `firefox`; `shell:AppsFolder\Chrome` gives
/// `chrome`; a Store app's id (`Pkg_x!App`) gives nothing, so its name or title must match.
pub(crate) fn process_hint(program: &str) -> String {
    let target = program
        .strip_prefix("shell:AppsFolder\\")
        .unwrap_or(program);
    if target.contains('!') {
        return String::new();
    }
    let last = target.rsplit(['\\', '/']).next().unwrap_or(target);
    let stem = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    stem.to_lowercase()
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
    fn finds_the_process_name_behind_a_launch_value() {
        assert_eq!(process_hint("firefox.exe"), "firefox");
        assert_eq!(process_hint("C:/Program Files/VLC/VLC.exe"), "vlc");
        assert_eq!(process_hint("code.cmd"), "code");
        assert_eq!(process_hint("shell:AppsFolder\\Chrome"), "chrome");
        assert_eq!(
            process_hint("shell:AppsFolder\\{6D80}\\Mozilla Firefox\\firefox.exe"),
            "firefox"
        );
        assert_eq!(
            process_hint("shell:AppsFolder\\Microsoft.WindowsCalculator_8we!App"),
            ""
        );
    }

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
