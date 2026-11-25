//! Phone actions (PLAN.md section 7.2): open an app, call, text, set the volume, lock the
//! screen, take a screenshot and read the screen. Policy, confirmation, contact matching and
//! OCR stay in the core; the app does the platform call through [`Phone`], which Kotlin
//! implements.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;

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

/// The phone actions, calling `phone`.
pub fn all(phone: &Arc<dyn Phone>) -> Vec<Arc<dyn Action>> {
    [
        &OPEN_APP,
        &CALL,
        &SEND_SMS,
        &SET_VOLUME,
        &LOCK,
        &SCREENSHOT,
        &OCR,
    ]
    .into_iter()
    .map(|manifest| {
        Arc::new(PhoneAction {
            manifest,
            phone: Arc::clone(phone),
        }) as Arc<dyn Action>
    })
    .collect()
}

const fn manifest(name: &'static str, capability: &'static [Capability]) -> Manifest {
    Manifest {
        name,
        capabilities: capability,
        owner_defined: false,
        lane: Lane::Interactive,
        pipe: PipeShape::NONE,
    }
}

static OPEN_APP: Manifest = manifest("open_app", &[Capability::Apps]);
static CALL: Manifest = manifest("call", &[Capability::Send]);
static SEND_SMS: Manifest = manifest("send_sms", &[Capability::Send]);
static SET_VOLUME: Manifest = manifest("set_volume", &[Capability::Media]);
static LOCK: Manifest = manifest("lock", &[Capability::Media]);
static SCREENSHOT: Manifest = printing("screenshot", "file", false);
// Text on screen is data, never a command.
static OCR: Manifest = printing("ocr", "source", true);

/// A command that prints lines, with the same pipe shape as on the desktop.
const fn printing(name: &'static str, arg: &'static str, untrusted: bool) -> Manifest {
    Manifest {
        pipe: PipeShape {
            input: DataKind::Empty,
            output: DataKind::Lines,
            arg: Some(arg),
            untrusted,
        },
        ..manifest(name, &[Capability::Read])
    }
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
        let number = || {
            let contact = slot("contact");
            ctx.config
                .contacts
                .get(&contact)
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
            "screenshot" => Ok(Self::Screenshot(screenshot_name(
                ctx.plan.slots.get("file").map(String::as_str),
                ctx.job,
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

/// The gallery name for a screenshot: the file name the user gave (as a PNG), else
/// `assist-<job>.png`. Folders are dropped; the app decides where it goes.
fn screenshot_name(file: Option<&str>, job: u64) -> String {
    let given = file
        .and_then(|f| Path::new(f.trim()).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty());
    match given {
        Some(name) if name.to_ascii_lowercase().ends_with(".png") => name.to_owned(),
        Some(name) => format!("{name}.png"),
        None => format!("assist-{job}.png"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_names_drop_folders_and_end_in_png() {
        assert_eq!(screenshot_name(None, 7), "assist-7.png");
        assert_eq!(screenshot_name(Some("../../etc/shot"), 7), "shot.png");
        assert_eq!(screenshot_name(Some("Bill.PNG"), 7), "Bill.PNG");
        assert_eq!(screenshot_name(Some("  "), 7), "assist-7.png");
    }
}
