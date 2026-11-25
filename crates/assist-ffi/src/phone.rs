//! Phone actions (PLAN.md section 7.2): open an app, call, text and set the volume. Policy,
//! confirmation and contact matching stay in the core; the app does the platform call through
//! [`Phone`], which Kotlin implements.

use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::policy::Capability;

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
    [&OPEN_APP, &CALL, &SEND_SMS, &SET_VOLUME]
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
            other => Err(ActionError::new(format!("not a phone command: {other}"))),
        }
    }

    fn send(self, phone: &dyn Phone) -> Result<(), PhoneError> {
        match self {
            Self::OpenApp(package) => phone.open_app(package),
            Self::Call(number) => phone.call(number),
            Self::Sms(number, text) => phone.send_sms(number, text),
            Self::Volume(level) => phone.set_volume(level),
        }
    }
}

impl Action for PhoneAction {
    fn manifest(&self) -> &Manifest {
        self.manifest
    }

    fn run(&self, ctx: JobCtx, _input: Input, _out: Output) -> BoxFuture<Result<(), ActionError>> {
        let request = Request::from_ctx(self.manifest.name, &ctx);
        let phone = Arc::clone(&self.phone);
        Box::pin(async move {
            let request = request?;
            // The app's side calls into Android and may block on it.
            tokio::task::spawn_blocking(move || request.send(phone.as_ref()))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .map_err(|err| ActionError::new(err.to_string()))
        })
    }
}
