//! Recording with ffmpeg, run as an external process (PLAN.md section 4):
//! `record screen|audio [for 30s] [to file]`. Files are Matroska (`.mkv` / `.mka`), which
//! stays playable when a recording is stopped; without `for`, it runs until the job is
//! stopped.
//!
//! shortcut: a recording also ends at `[jobs] timeout` (10 minutes by default); upgrade to
//! a per-command timeout when long recordings are needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::config::parse_duration;
use assist_core::policy::Capability;
use tokio::process::Command;

use crate::process;

#[cfg(unix)]
#[path = "record_unix.rs"]
mod os;
#[cfg(windows)]
#[path = "record_windows.rs"]
mod os;

/// Longest recording `for` accepts.
const MAX_FOR: Duration = Duration::from_secs(4 * 3600);

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

/// What to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Screen,
    Audio,
}

#[derive(Debug, PartialEq)]
struct Spec {
    kind: Kind,
    length: Option<Duration>,
    file: Option<String>,
}

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

impl Spec {
    /// `screen|audio [for <length>] [to <file>]`; the file keeps its spelling.
    fn parse(text: &str) -> Result<Self, String> {
        let usage = || "use: record screen|audio [for 30s] [to file]".to_owned();
        let (head, file) = match text.split_once(" to ") {
            Some((head, file)) if !file.trim().is_empty() => (head, Some(file.trim().to_owned())),
            _ => (text, None),
        };
        let words: Vec<String> = head.split_whitespace().map(str::to_lowercase).collect();
        let kind = words
            .iter()
            .find_map(|w| match w.as_str() {
                "screen" | "desktop" => Some(Kind::Screen),
                "audio" | "mic" | "microphone" | "sound" | "voice" => Some(Kind::Audio),
                _ => None,
            })
            .ok_or_else(usage)?;
        let length = match words.iter().position(|w| w == "for") {
            Some(at) => Some(length(&words[at + 1..]).ok_or_else(usage)?),
            None => None,
        };
        if length.is_some_and(|l| l.is_zero() || l > MAX_FOR) {
            return Err("record for at most 4h".to_owned());
        }
        Ok(Self { kind, length, file })
    }
}

/// `30s`, `30 seconds`, `2 min`, `1 hour`.
fn length(words: &[String]) -> Option<Duration> {
    let joined = words.join(" ");
    if let Some(d) = parse_duration(&joined) {
        return Some(d);
    }
    let (n, unit) = (words.first()?, words.get(1)?);
    let unit = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => "s",
        "m" | "min" | "mins" | "minute" | "minutes" => "m",
        "h" | "hour" | "hours" => "h",
        _ => return None,
    };
    parse_duration(&format!("{n}{unit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_how_long_and_where() {
        let s = Spec::parse("screen for 30s to Demo Video.mkv").unwrap();
        assert_eq!(
            s,
            Spec {
                kind: Kind::Screen,
                length: Some(Duration::from_secs(30)),
                file: Some("Demo Video.mkv".to_owned())
            }
        );
        let s = Spec::parse("the microphone for 2 minutes").unwrap();
        assert_eq!(
            (s.kind, s.length),
            (Kind::Audio, Some(Duration::from_secs(120)))
        );
        assert_eq!(Spec::parse("audio").unwrap().length, None);
        assert!(Spec::parse("the garden").is_err());
        assert!(Spec::parse("screen for ever").is_err());
        assert!(Spec::parse("screen for 9h").is_err());
    }
}
