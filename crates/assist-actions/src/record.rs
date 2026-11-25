//! Recording with ffmpeg, run as an external process (PLAN.md section 4):
//! `record screen|audio [for 30s] [to file]`. Files are Matroska (`.mkv` / `.mka`), which
//! stays playable when a recording is stopped; without `for`, it runs until the job is
//! stopped.
//!
//! shortcut: a recording also ends at `[jobs] timeout` (10 minutes by default); upgrade to
//! a per-command timeout when long recordings are needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;
use tokio::process::Command;

use crate::process;
use crate::record_spec::{Kind, Spec};

#[cfg(unix)]
#[path = "record_unix.rs"]
mod os;
#[cfg(windows)]
#[path = "record_windows.rs"]
mod os;

/// `record`.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Record)]
}

/// Records the screen or the microphone.
pub struct Record;

static RECORD: Manifest = Manifest {
    name: "record",
    capabilities: &[Capability::Record],
    owner_defined: false,
    lane: Lane::Background,
    pipe: PipeShape {
        input: DataKind::Empty,
        output: DataKind::Empty,
        arg: Some("spec"),
        untrusted: false,
    },
};

impl Action for Record {
    fn manifest(&self) -> &Manifest {
        &RECORD
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let spec = Spec::parse(ctx.plan.slots.get("spec").map_or("", String::as_str))
                .map_err(ActionError::new)?;
            if !process::on_path(process::FFMPEG) {
                return Err(ActionError::new(format!(
                    "ffmpeg is not installed; install it ({}) and try again",
                    process::FFMPEG_INSTALL
                )));
            }
            let path = match &spec.file {
                Some(file) => ctx.cwd.join(crate::expand_home(file)),
                None => default_path(spec.kind, ctx.job)?,
            };
            let mut args = vec![
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-nostdin".into(),
            ];
            args.extend(os::input(spec.kind).await.map_err(ActionError::new)?);
            if let Some(length) = spec.length {
                args.extend(["-t".to_owned(), length.as_secs().to_string()]);
            }
            args.extend(encode(spec.kind));
            args.push("-y".into());
            args.push(path.display().to_string());
            let what = match spec.kind {
                Kind::Screen => "the screen",
                Kind::Audio => "audio",
            };
            let _ = out
                .send(Chunk::Info(format!(
                    "Recording {what} to {}; stop the job to end it.",
                    path.display()
                )))
                .await;
            let mut command = Command::new(process::FFMPEG);
            command.args(&args);
            process::run(command, &ctx.cwd, &out).await
        })
    }
}

/// Codecs per kind: fast H.264 for the screen, plain PCM for audio.
fn encode(kind: Kind) -> Vec<String> {
    let args: &[&str] = match kind {
        Kind::Screen => &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ],
        Kind::Audio => &["-c:a", "pcm_s16le"],
    };
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `Videos/assist-<job>.mkv` or `Music/assist-<job>.mka`.
fn default_path(kind: Kind, job: u64) -> Result<PathBuf, ActionError> {
    let dirs = directories::UserDirs::new().ok_or_else(|| ActionError::new("no home folder"))?;
    let (dir, ext) = match kind {
        Kind::Screen => (dirs.video_dir(), "mkv"),
        Kind::Audio => (dirs.audio_dir(), "mka"),
    };
    let dir: &Path =
        dir.ok_or_else(|| ActionError::new("no Videos or Music folder; give a file name"))?;
    Ok(dir.join(format!("assist-{job}.{ext}")))
}
