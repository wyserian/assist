//! Typed bodies for each [`crate::Kind`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::job::{JobState, Lane};

/// [`crate::Kind::Hello`]: who is on the other end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Program name, e.g. `assist-cli` or `assist-web`.
    pub client: String,
    /// Program version.
    pub version: String,
    /// The client's working directory; shell jobs from this connection start there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Utterances come from a microphone: the daemon gives this connection voice trust
    /// (anyone in the room). A client can only lower its trust this way, never raise it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub voice: bool,
    /// A paired phone's contact names, so the hub understands "text mom"; the numbers stay
    /// on the phone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<String>,
}

/// [`crate::Kind::Pair`]: a new device asks to be paired. Sent over an authenticated
/// connection, so the hub also knows the device's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    /// The one-time secret from the hub's pairing code.
    pub secret: String,
    /// The device's name, e.g. `Pixel 5`.
    pub name: String,
    /// What it can do for the hub, e.g. `call`, `send_sms` (commands it runs).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// [`crate::Kind::Paired`]: the hub accepted the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paired {
    /// The hub's name, e.g. its computer name.
    pub name: String,
}

/// [`crate::Kind::Run`]: run this confirmed command on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The hub's number for this request, echoed in [`Ran`].
    pub job: u64,
    /// The command, e.g. `send_sms`.
    pub command: String,
    /// Its resolved slots.
    #[serde(default)]
    pub slots: std::collections::BTreeMap<String, String>,
}

/// [`crate::Kind::Ran`]: how a [`Run`] went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ran {
    /// The [`Run`]'s number.
    pub job: u64,
    /// What it printed.
    #[serde(default)]
    pub lines: Vec<String>,
    /// Why it failed; `None` when it worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// [`crate::Kind::Utterance`]: text from the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utterance {
    /// What the user typed or said.
    pub text: String,
    /// The voice matched the owner's voiceprint, so a spoken "yes" counts. Only a client
    /// that already holds the token can send this, so it grants nothing it could not do.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub speaker_verified: bool,
}

/// [`crate::Kind::Reply`]: the assistant's answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Lines to show or speak, in order.
    #[serde(default)]
    pub say: Vec<String>,
    /// Jobs started by this utterance.
    #[serde(default)]
    pub jobs: Vec<JobBrief>,
    /// The assistant asked a question and waits for the answer in this conversation.
    #[serde(default)]
    pub waiting: bool,
}

/// A job just started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobBrief {
    /// Job id.
    pub id: u64,
    /// The resolved action, as read back to the user.
    pub title: String,
}

/// Which output stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Normal output.
    Out,
    /// Error output.
    Err,
    /// A status message from the action itself.
    Info,
}

/// [`crate::Kind::JobUpdate`]: something happened to a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobUpdate {
    /// The job changed state.
    State {
        /// Job id.
        id: u64,
        /// New state.
        state: JobState,
        /// Why it failed, for failed jobs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The job produced a line of output.
    Output {
        /// Job id.
        id: u64,
        /// Which stream.
        stream: Stream,
        /// The line.
        text: String,
    },
}

/// [`crate::Kind::Subscribe`]: which updates to receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    /// Topics: `jobs` sends every [`JobUpdate`], `notices` every [`Notice`].
    pub topics: Vec<String>,
}

/// [`crate::Kind::Notice`]: a proactive message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// The rule that sent it, for "stop telling me about ...".
    pub rule: String,
    /// What to say.
    pub text: String,
}

/// [`crate::Kind::Rpc`]: a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rpc {
    /// Method name, e.g. `jobs.list`.
    pub method: String,
    /// Method-specific parameters.
    #[serde(default)]
    pub params: Value,
}

/// [`crate::Kind::RpcResult`]: the answer to an [`Rpc`]. Exactly one of `result` and `error`
/// is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResult {
    /// The result on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResult {
    /// A successful result.
    pub fn ok(result: impl Serialize) -> Self {
        Self {
            result: Some(serde_json::to_value(result).unwrap_or(Value::Null)),
            error: None,
        }
    }

    /// A failure.
    pub fn err(code: &str, message: impl Into<String>) -> Self {
        Self {
            result: None,
            error: Some(RpcError {
                code: code.to_owned(),
                message: message.into(),
            }),
        }
    }
}

/// Why an [`Rpc`] failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// Machine-readable code, e.g. `not_found`, `denied`, `bad_request`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

/// One job in `jobs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSummary {
    /// Job id.
    pub id: u64,
    /// Command name.
    pub command: String,
    /// The resolved action, as read back to the user.
    pub title: String,
    /// The adapter that asked, e.g. `cli`.
    pub source: String,
    /// Lane.
    pub lane: Lane,
    /// Current state.
    pub state: JobState,
    /// Submitted, Unix ms.
    pub created: u64,
    /// Started, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
    /// Ended, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<u64>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The first job of the pipe this job is a stage of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
    /// CPU use of its processes in whole percent of the machine, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    /// Memory of its processes in bytes, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_update_is_tagged() {
        let update = JobUpdate::Output {
            id: 3,
            stream: Stream::Err,
            text: "oops".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&update).unwrap(),
            r#"{"type":"output","id":3,"stream":"err","text":"oops"}"#
        );
        let state: JobUpdate =
            serde_json::from_str(r#"{"type":"state","id":3,"state":"done","new":1}"#).unwrap();
        assert_eq!(
            state,
            JobUpdate::State {
                id: 3,
                state: JobState::Done,
                error: None
            }
        );
    }

    #[test]
    fn rpc_result_has_one_side() {
        let ok = serde_json::to_string(&RpcResult::ok(5)).unwrap();
        assert_eq!(ok, r#"{"result":5}"#);
        let err = serde_json::to_string(&RpcResult::err("not_found", "no job 9")).unwrap();
        assert_eq!(
            err,
            r#"{"error":{"code":"not_found","message":"no job 9"}}"#
        );
    }
}
