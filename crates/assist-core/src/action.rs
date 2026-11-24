//! The plugin contract every action implements (PLAN.md section 3.9).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::dialog::Plan;
use crate::policy::{Capability, Principal};
use crate::scheduler::JobId;

/// A boxed, sendable future, as returned by [`Action::run`].
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Data flowing into an action from the previous pipe stage; closed when there is none.
pub type Input = mpsc::Receiver<Chunk>;

/// Where an action streams its output. Bounded, so a fast producer waits for slow readers.
pub type Output = mpsc::Sender<Chunk>;

/// One piece of streamed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// A line of normal output (stdout).
    Line(String),
    /// A line of error output (stderr).
    Error(String),
    /// A status message from the action itself, e.g. "Opened firefox".
    Info(String),
    /// The shell's working directory for this conversation changed.
    Cwd(PathBuf),
}

pub use assist_proto::Lane;

/// What flows between pipe stages (PLAN.md section 3.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    /// Nothing: the action reads no input, or writes no lines for a next stage.
    Empty,
    /// Text lines ([`Chunk::Line`]).
    Lines,
}

/// How an action fits in a pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeShape {
    /// What it reads from the previous stage. `Lines` makes it a transform: written after a
    /// `|` as `<name> <arg>`, never matched from speech.
    pub input: DataKind,
    /// What it writes for the next stage.
    pub output: DataKind,
    /// For a transform, the slot that gets the rest of the stage text, e.g. `count` in
    /// `head 3`.
    pub arg: Option<&'static str>,
    /// The output comes from outside the owner's control (OCR, web pages, messages); a
    /// later stage that sends always asks (taint rule, PLAN.md section 3.6).
    pub untrusted: bool,
}

impl PipeShape {
    /// Reads nothing, writes nothing a next stage can use (opening an app).
    pub const NONE: Self = Self {
        input: DataKind::Empty,
        output: DataKind::Empty,
        arg: None,
        untrusted: false,
    };
    /// Starts a pipe: reads nothing, writes lines (a script's output).
    pub const SOURCE: Self = Self {
        input: DataKind::Empty,
        output: DataKind::Lines,
        arg: None,
        untrusted: false,
    };

    /// A transform from lines to `output`, taking the rest of the stage as `arg`.
    pub const fn transform(output: DataKind, arg: Option<&'static str>) -> Self {
        Self {
            input: DataKind::Lines,
            output,
            arg,
            untrusted: false,
        }
    }

    /// Whether this is a transform, which needs a stage before it.
    pub fn is_transform(&self) -> bool {
        self.input == DataKind::Lines
    }
}

/// What an action is and needs. The core refuses capabilities that are not declared.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The command this action runs, e.g. `open_app`.
    pub name: &'static str,
    /// Capabilities the policy checks.
    pub capabilities: &'static [Capability],
    /// Runs only what the owner wrote in config (named scripts); counts as typed.
    pub owner_defined: bool,
    /// Default lane.
    pub lane: Lane,
    /// How it fits in a pipe.
    pub pipe: PipeShape,
}

/// Everything a running job gets.
#[derive(Debug, Clone)]
pub struct JobCtx {
    /// This job.
    pub job: JobId,
    /// The confirmed plan; its slots are frozen.
    pub plan: Plan,
    /// Who asked.
    pub principal: Principal,
    /// Config, for values such as app paths and scripts.
    pub config: Arc<Config>,
    /// Working directory for shell-like actions.
    pub cwd: PathBuf,
    /// Cancelled on stop, timeout and STOP ALL. Dropping the future also stops the job.
    pub cancel: CancellationToken,
}

/// Why an action failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActionError {
    /// Human-readable reason.
    pub message: String,
    /// Process exit code, when there is one.
    pub exit: Option<i32>,
}

impl ActionError {
    /// A failure with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit: None,
        }
    }

    /// A process that exited with a non-zero code.
    pub fn exit(code: i32) -> Self {
        Self {
            message: format!("exited with code {code}"),
            exit: Some(code),
        }
    }
}

/// A built-in or external action.
pub trait Action: Send + Sync {
    /// Name, capabilities and defaults.
    fn manifest(&self) -> &Manifest;

    /// The lane for this plan; defaults to the manifest's.
    fn lane(&self, _plan: &Plan, _config: &Config) -> Lane {
        self.manifest().lane
    }

    /// Runs the job, streaming output to `out`. Must stop promptly when `ctx.cancel` fires
    /// or the future is dropped (processes are killed on drop).
    fn run(&self, ctx: JobCtx, input: Input, out: Output) -> BoxFuture<Result<(), ActionError>>;
}
