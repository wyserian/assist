//! Phone actions (PLAN.md section 7.2): open an app, call, text, set the volume, lock the
//! screen, take a screenshot, read the screen and record. Policy, confirmation, contact
//! matching, OCR and how long a recording runs stay in the core; the app does the platform
//! call through [`Phone`], which Kotlin implements.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assist_actions::record_spec::{Kind, Spec};
use assist_core::action::{Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Manifest, Output};
use assist_core::normalize::normalize;
use assist_net::commands::{PHONE, RECORD};

use crate::models::{have, models_dir, Feature};

/// What the app does for the core; each call runs on a blocking thread.
#[uniffi::export(with_foreign)]
pub trait Phone: Send + Sync {
    /// Launches the app with this package name.
    fn open_app(&self, package_name: String) -> Result<(), PhoneError>;
    /// Calls this number.
    fn call(&self, number: String) -> Result<(), PhoneError>;
    /// Sends `text` to this number as an SMS.
    fn send_sms(&self, number: String, text: String) -> Result<(), PhoneError>;
    /// Sets the media volume, 0 to 100.
    fn set_volume(&self, percent: u8) -> Result<(), PhoneError>;
    /// Locks the screen.
    fn lock(&self) -> Result<(), PhoneError>;
    /// Saves a screenshot named `name` in the gallery; returns where it went, for the user.
    fn screenshot(&self, name: String) -> Result<String, PhoneError>;
    /// Captures the screen to a PNG file for the core to read; returns its path. The core
    /// removes it.
    fn capture(&self) -> Result<String, PhoneError>;
    /// Asks the user to download the models `feature` needs.
    fn offer_models(&self, feature: Feature) -> Result<(), PhoneError>;
    /// Starts recording `what` into a gallery file named `name` for job `job` (so the app's
    /// Stop can stop that job); returns where it goes. Screen recording asks the user first.
    fn start_recording(
        &self,
        what: Recording,
        name: String,
        job: u64,
    ) -> Result<String, PhoneError>;
    /// Stops the recording and saves it; returns where it went.
    fn stop_recording(&self) -> Result<String, PhoneError>;
    /// A proactive message ("battery is low"): spoken when `speak`, else a notification.
    fn notice(&self, text: String, speak: bool) -> Result<(), PhoneError>;
    /// A line from the paired computer (its jobs' output) for the chat.
    fn show(&self, text: String) -> Result<(), PhoneError>;
}

/// What to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Recording {
    /// The screen, without sound.
    Screen,
    /// The microphone.
    Audio,
}

/// Why the phone could not do it.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PhoneError {
    /// A reason to show the user, such as a missing permission.
    #[error("{reason}")]
    Failed {
        /// What went wrong.
        reason: String,
    },
}

impl From<uniffi::UnexpectedUniFFICallbackError> for PhoneError {
    fn from(err: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failed { reason: err.reason }
    }
}

/// What the phone can do for a paired computer: its commands' names.
pub(crate) fn capabilities() -> Vec<String> {
    PHONE
        .iter()
        .map(|manifest| manifest.name.to_owned())
        .collect()
}

/// The phone actions, calling `phone`.
pub fn all(phone: &Arc<dyn Phone>) -> Vec<Arc<dyn Action>> {
    PHONE
        .into_iter()
        .filter(|manifest| manifest.name != RECORD.name)
        .map(|manifest| {
            Arc::new(PhoneAction {
                manifest,
                phone: Arc::clone(phone),
            }) as Arc<dyn Action>
        })
        .chain([Arc::new(Record {
            phone: Arc::clone(phone),
        }) as Arc<dyn Action>])
        .collect()
}

/// One phone command; `manifest.name` says which.
struct PhoneAction {
    manifest: &'static Manifest,
    phone: Arc<dyn Phone>,
}

/// A resolved phone command.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    OpenApp(String),
    Call(String),
    Sms(String, String),
    Volume(u8),
    Lock,
    Screenshot(String),
    Ocr(PathBuf),
}

impl Request {
    /// Reads the frozen slots and the numbers and packages behind them.
    fn from_ctx(command: &str, ctx: &JobCtx) -> Result<Self, ActionError> {
        let slot = |name: &str| ctx.plan.slots.get(name).cloned().unwrap_or_default();
        // A computer sends the contact as it matched it ("mom" for the phone's "Mom").
        let number = || {
            let contact = slot("contact");
            let wanted = normalize(&contact);
            ctx.config
                .contacts
                .get(&contact)
                .or_else(|| {
                    let mut all = ctx.config.contacts.iter();
                    all.find(|(name, _)| normalize(name) == wanted)
                        .map(|(_, n)| n)
                })
                .cloned()
                .ok_or_else(|| ActionError::new(format!("no number for {contact:?}")))
        };
        match command {
            "open_app" => {
                let app = slot("app");
                ctx.config
                    .apps
                    .get(&app)
                    .and_then(|v| v.as_str().or_else(|| v.get("android")?.as_str()))
                    .map(|package| Self::OpenApp(package.to_owned()))
                    .ok_or_else(|| ActionError::new(format!("{app:?} is not an app on this phone")))
            }
            "call" => Ok(Self::Call(number()?)),
            "send_sms" => Ok(Self::Sms(number()?, slot("text"))),
            "set_volume" => slot("level")
                .parse::<u8>()
                .ok()
                .filter(|level| *level <= 100)
                .map(Self::Volume)
                .ok_or_else(|| ActionError::new("the volume goes from 0 to 100")),
            "lock" => Ok(Self::Lock),
            "screenshot" => Ok(Self::Screenshot(gallery_name(
                ctx.plan.slots.get("file").map(String::as_str),
                ctx.job,
                "png",
            ))),
            "ocr" => match ctx.plan.slots.get("source").map(|s| s.trim()) {
                None | Some("" | "screen" | "the screen") => ctx
                    .data_dir
                    .as_deref()
                    .map(|data| Self::Ocr(models_dir(data)))
                    .ok_or_else(|| ActionError::new("reading the screen needs the data directory")),
                Some(_) => Err(ActionError::new("On the phone I can read the screen only.")),
            },
            other => Err(ActionError::new(format!("not a phone command: {other}"))),
        }
    }

    /// Does it; returns the lines to print.
    fn send(self, phone: &dyn Phone) -> Result<Vec<String>, PhoneError> {
        let done = |result: Result<(), PhoneError>| result.map(|()| Vec::new());
        match self {
            Self::OpenApp(package) => done(phone.open_app(package)),
            Self::Call(number) => done(phone.call(number)),
            Self::Sms(number, text) => done(phone.send_sms(number, text)),
            Self::Volume(level) => done(phone.set_volume(level)),
            Self::Lock => done(phone.lock()),
            Self::Screenshot(name) => phone.screenshot(name).map(|saved| vec![saved]),
            Self::Ocr(models) => read_screen(phone, &models),
        }
    }
}

/// The gallery name for a file the user asked for: the name they gave (with extension `ext`),
/// else `assist-<job>.<ext>`. Folders are dropped; the app decides where it goes.
fn gallery_name(file: Option<&str>, job: u64, ext: &str) -> String {
    let given = file
        .and_then(|f| Path::new(f.trim()).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty());
    match given {
        Some(name) if name.to_ascii_lowercase().ends_with(&format!(".{ext}")) => name.to_owned(),
        Some(name) => format!("{name}.{ext}"),
        None => format!("assist-{job}.{ext}"),
    }
}

/// Captures the screen and reads it; offers the OCR models first if they are missing.
fn read_screen(phone: &dyn Phone, models: &Path) -> Result<Vec<String>, PhoneError> {
    if !have(models, Feature::Ocr) {
        phone.offer_models(Feature::Ocr)?;
        return Err(PhoneError::Failed {
            reason: "I need the text reading models first. Tap Download, then ask again."
                .to_owned(),
        });
    }
    let image = PathBuf::from(phone.capture()?);
    let lines = assist_actions::ocr::read(models, &image);
    let _ = std::fs::remove_file(&image);
    lines.map_err(|reason| PhoneError::Failed { reason })
}

impl Action for PhoneAction {
    fn manifest(&self) -> &Manifest {
        self.manifest
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let request = Request::from_ctx(self.manifest.name, &ctx);
        let phone = Arc::clone(&self.phone);
        Box::pin(async move {
            let request = request?;
            // The app's side calls into Android and may block on it; OCR is heavy too.
            let lines = tokio::task::spawn_blocking(move || request.send(phone.as_ref()))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .map_err(|err| ActionError::new(err.to_string()))?;
            for line in lines {
                if out.send(Chunk::Line(line)).await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

/// `record screen|audio [for <length>] [to <file>]` on the phone: the core keeps the length
/// and the stop; the app records into the gallery.
struct Record {
    phone: Arc<dyn Phone>,
}

/// Stops the app's recorder when the job ends any way, also when the scheduler drops the
/// job on stop, timeout or STOP ALL.
struct Recorder {
    phone: Option<Arc<dyn Phone>>,
}

impl Recorder {
    /// Stops and saves; returns where it went.
    async fn finish(mut self) -> Result<String, ActionError> {
        let Some(phone) = self.phone.take() else {
            return Err(ActionError::new("the recording already stopped"));
        };
        tokio::task::spawn_blocking(move || phone.stop_recording())
            .await
            .map_err(|err| ActionError::new(err.to_string()))?
            .map_err(|err| ActionError::new(err.to_string()))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(phone) = self.phone.take() {
            // Off this thread: the app's side may block briefly while it saves.
            drop(tokio::task::spawn_blocking(move || phone.stop_recording()));
        }
    }
}

impl Action for Record {
    fn manifest(&self) -> &Manifest {
        &RECORD
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let phone = Arc::clone(&self.phone);
        Box::pin(async move {
            let spec = Spec::parse(ctx.plan.slots.get("spec").map_or("", String::as_str))
                .map_err(ActionError::new)?;
            let (what, ext, said) = match spec.kind {
                Kind::Screen => (Recording::Screen, "mp4", "the screen"),
                Kind::Audio => (Recording::Audio, "m4a", "audio"),
            };
            let name = gallery_name(spec.file.as_deref(), ctx.job, ext);
            let job = ctx.job;
            let starter = Arc::clone(&phone);
            let target =
                tokio::task::spawn_blocking(move || starter.start_recording(what, name, job))
                    .await
                    .map_err(|err| ActionError::new(err.to_string()))?
                    .map_err(|err| ActionError::new(err.to_string()))?;
            let recorder = Recorder { phone: Some(phone) };
            let _ = out
                .send(Chunk::Info(format!(
                    "Recording {said} to {target}; stop the job to end it."
                )))
                .await;
            match spec.length {
                Some(length) => tokio::time::sleep(length).await,
                None => ctx.cancel.cancelled().await,
            }
            let saved = recorder.finish().await?;
            let _ = out.send(Chunk::Info(format!("Saved {saved}."))).await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_names_drop_folders_and_keep_the_extension() {
        assert_eq!(gallery_name(None, 7, "png"), "assist-7.png");
        assert_eq!(gallery_name(Some("../../etc/shot"), 7, "png"), "shot.png");
        assert_eq!(gallery_name(Some("Bill.PNG"), 7, "png"), "Bill.PNG");
        assert_eq!(gallery_name(Some("  "), 7, "png"), "assist-7.png");
        assert_eq!(gallery_name(Some("demo"), 9, "mp4"), "demo.mp4");
    }
}
