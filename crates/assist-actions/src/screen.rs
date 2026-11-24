//! The screen (PLAN.md section 3.9): `screenshot [file]` saves the primary monitor as a PNG
//! and prints its path; `ocr [screen | image file]` prints the text it reads. OCR output is
//! untrusted: text on screen is data, never a command, and a later `send` stage asks.
//!
//! shortcut: Windows only (xcap and the built-in Windows OCR); Linux needs xcap's PipeWire
//! and XCB libraries and an OCR engine (`ocrs` or Tesseract), added with Linux OCR.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;

#[path = "screen_windows.rs"]
mod os;

/// Screenshot and OCR.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Screenshot), Arc::new(Ocr)]
}

/// `screenshot [file]`.
pub struct Screenshot;

static SCREENSHOT: Manifest = Manifest {
    name: "screenshot",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape {
        input: DataKind::Empty,
        output: DataKind::Lines,
        arg: Some("file"),
        untrusted: false,
    },
};

/// `ocr [screen | image file]`.
pub struct Ocr;

static OCR: Manifest = Manifest {
    name: "ocr",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape {
        input: DataKind::Empty,
        output: DataKind::Lines,
        arg: Some("source"),
        untrusted: true,
    },
};

impl Action for Screenshot {
    fn manifest(&self) -> &Manifest {
        &SCREENSHOT
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let path = match ctx.plan.slots.get("file") {
                Some(file) => absolute(&ctx.cwd, file),
                None => default_path(ctx.job)?,
            };
            let saved = path.clone();
            blocking(move || os::screenshot(&saved)).await?;
            let _ = out.send(Chunk::Line(path.display().to_string())).await;
            Ok(())
        })
    }
}

impl Action for Ocr {
    fn manifest(&self) -> &Manifest {
        &OCR
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let source = ctx.plan.slots.get("source").map(|s| s.trim().to_owned());
            let lines = match source.as_deref() {
                None | Some("" | "screen" | "the screen") => blocking(os::ocr_screen).await?,
                Some(file) => {
                    let path = absolute(&ctx.cwd, file);
                    blocking(move || os::ocr_file(&path)).await?
                }
            };
            for line in lines {
                if out.send(Chunk::Line(line)).await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

/// `~/x` and relative paths resolved against the job's directory.
fn absolute(cwd: &Path, file: &str) -> PathBuf {
    cwd.join(crate::expand_home(file))
}

/// `Pictures/assist-<job>.png`.
fn default_path(job: u64) -> Result<PathBuf, ActionError> {
    let dir = directories::UserDirs::new()
        .and_then(|dirs| dirs.picture_dir().map(Path::to_path_buf))
        .ok_or_else(|| ActionError::new("no Pictures folder; give a file name"))?;
    Ok(dir.join(format!("assist-{job}.png")))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, ActionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ActionError::new(err.to_string()))?
        .map_err(ActionError::new)
}
