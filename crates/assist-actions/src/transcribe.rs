//! `transcribe <file>`: the speech in an audio file, one line per sentence, with the same
//! Kroko model as `listen` (models in `<data dir>/models`). WAV is read directly; other
//! formats (`record audio` writes `.mka`) are converted with ffmpeg first. The text is
//! untrusted: what someone said in a recording is data, never a command.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;
use assist_voice::{Recognizer, STT_MODEL, VAD_MODEL};

use crate::process;

/// `transcribe`.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Transcribe)]
}

/// Transcribes audio files.
pub struct Transcribe;

static TRANSCRIBE: Manifest = Manifest {
    name: "transcribe",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Background,
    pipe: PipeShape {
        input: DataKind::Empty,
        output: DataKind::Lines,
        arg: Some("file"),
        untrusted: true,
    },
};

impl Action for Transcribe {
    fn manifest(&self) -> &Manifest {
        &TRANSCRIBE
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let file = ctx.plan.slots.get("file").ok_or_else(|| {
                ActionError::new("transcribe needs a file, e.g. transcribe talk.wav")
            })?;
            let path = ctx.cwd.join(crate::expand_home(file));
            let data = ctx
                .data_dir
                .clone()
                .ok_or_else(|| ActionError::new("transcribe needs the data directory"))?;
            let models = data.join("models");
            if !models.join(STT_MODEL).exists() || !models.join(VAD_MODEL).exists() {
                return Err(ActionError::new(
                    "the voice models are missing; run assist listen once to download them",
                ));
            }
            let is_wav = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"));
            let converted = if is_wav {
                None
            } else {
                Some(convert(&path, &data.join(format!("transcribe-{}.wav", ctx.job))).await?)
            };
            let wav = converted.clone().unwrap_or(path);
            let lines = tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
                let samples = assist_voice::read_wav(&wav).map_err(|e| e.to_string())?;
                let recognizer =
                    Recognizer::new(&models.join(STT_MODEL)).map_err(|e| e.to_string())?;
                assist_voice::transcribe(&samples, &models.join(VAD_MODEL), &recognizer)
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|err| ActionError::new(err.to_string()));
            if let Some(tmp) = converted {
                let _ = tokio::fs::remove_file(tmp).await;
            }
            for line in lines?.map_err(ActionError::new)? {
                if out.send(Chunk::Line(line)).await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

/// Converts any audio ffmpeg reads into a 16 kHz mono WAV at `to`.
async fn convert(from: &Path, to: &Path) -> Result<PathBuf, ActionError> {
    if !process::on_path(process::FFMPEG) {
        return Err(ActionError::new(format!(
            "only WAV works without ffmpeg; install it ({}) or convert the file",
            process::FFMPEG_INSTALL
        )));
    }
    let status = tokio::process::Command::new(process::FFMPEG)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(from)
        .args(["-ac", "1", "-ar", "16000"])
        .arg(to)
        .stdin(Stdio::null())
        .status()
        .await
        .map_err(|err| ActionError::new(format!("cannot run ffmpeg: {err}")))?;
    if !status.success() {
        let _ = tokio::fs::remove_file(to).await;
        return Err(ActionError::new(format!(
            "ffmpeg cannot read {}",
            from.display()
        )));
    }
    Ok(to.to_owned())
}
