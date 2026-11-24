//! The assist wire format (PLAN.md section 3.3): JSON envelopes over WebSocket.
//!
//! Every message is an [`Envelope`] with a version, an id, an optional reply-to id and
//! conversation id, a [`Kind`] and a body. Unknown fields are ignored and unknown kinds
//! decode as [`Kind::Unknown`], so new fields and kinds never break old clients. A different
//! major version `v` is rejected.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use uuid::Uuid;

mod body;
mod job;
pub mod rpc;

pub use body::{
    Hello, JobBrief, JobSummary, JobUpdate, Notice, Reply, Rpc, RpcError, RpcResult, Stream,
    Subscribe, Utterance,
};
pub use job::{JobState, Lane};

/// Protocol major version.
pub const VERSION: u16 = 1;

/// Largest accepted frame; bigger frames are rejected before parsing.
pub const MAX_FRAME: usize = 1024 * 1024;

/// What a message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// First message on a connection, both ways: who is talking.
    Hello,
    /// Text from the user.
    Utterance,
    /// The assistant's answer to an utterance.
    Reply,
    /// A job changed state or produced output.
    JobUpdate,
    /// Something the assistant says unasked (PLAN.md section 3.17); sent to clients that
    /// subscribed to `notices`.
    Notice,
    /// Start receiving job updates.
    Subscribe,
    /// A request: list jobs, stop a job, set a secret, ...
    Rpc,
    /// The answer to an [`Kind::Rpc`].
    RpcResult,
    /// A kind this build does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// One message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Protocol major version, [`VERSION`].
    pub v: u16,
    /// This message.
    pub id: Uuid,
    /// The message this one answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<Uuid>,
    /// The conversation; the server keeps dialog state per conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv: Option<Uuid>,
    /// What the message is.
    pub kind: Kind,
    /// Sending node or client.
    pub from: String,
    /// Receiving node, or `hub`.
    pub to: String,
    /// Unix time in milliseconds.
    pub ts: u64,
    /// Kind-specific body; see [`Envelope::body`].
    #[serde(default)]
    pub body: Value,
}

/// Why a frame could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The frame is larger than [`MAX_FRAME`].
    #[error("frame of {0} bytes is too large")]
    TooLarge(usize),
    /// Not a valid envelope.
    #[error("invalid message: {0}")]
    Invalid(#[from] serde_json::Error),
    /// Another protocol major version.
    #[error("protocol version {0} is not supported (this build speaks {VERSION})")]
    Version(u16),
}

impl Envelope {
    /// A new message from `from` to `to`.
    pub fn new(kind: Kind, from: &str, to: &str, body: impl Serialize) -> Self {
        Self {
            v: VERSION,
            id: Uuid::new_v4(),
            re: None,
            conv: None,
            kind,
            from: from.to_owned(),
            to: to.to_owned(),
            ts: now_ms(),
            body: serde_json::to_value(body).unwrap_or(Value::Null),
        }
    }

    /// An answer to this message: same conversation, `re` set, addressed back.
    pub fn answer(&self, kind: Kind, from: &str, body: impl Serialize) -> Self {
        Self {
            re: Some(self.id),
            conv: self.conv,
            ..Self::new(kind, from, &self.from, body)
        }
    }

    /// The same message in conversation `conv`.
    pub fn in_conversation(self, conv: Uuid) -> Self {
        Self {
            conv: Some(conv),
            ..self
        }
    }

    /// The body as `T`.
    pub fn body<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.body)
    }

    /// One JSON text frame.
    pub fn encode(&self) -> String {
        // An envelope always serializes: every field is a plain value.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parses one JSON text frame.
    pub fn decode(frame: &str) -> Result<Self, DecodeError> {
        if frame.len() > MAX_FRAME {
            return Err(DecodeError::TooLarge(frame.len()));
        }
        let envelope: Self = serde_json::from_str(frame)?;
        if envelope.v != VERSION {
            return Err(DecodeError::Version(envelope.v));
        }
        Ok(envelope)
    }
}

/// Current Unix time in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_everything() {
        let hello = Envelope::new(
            Kind::Utterance,
            "cli",
            "hub",
            Utterance {
                text: "open firefox".to_owned(),
                speaker_verified: false,
            },
        )
        .in_conversation(Uuid::new_v4());
        let decoded = Envelope::decode(&hello.encode()).unwrap();
        assert_eq!(decoded, hello);
        assert_eq!(decoded.body::<Utterance>().unwrap().text, "open firefox");
    }

    #[test]
    fn answer_links_back() {
        let question = Envelope::new(Kind::Rpc, "cli", "hub", ()).in_conversation(Uuid::new_v4());
        let answer = question.answer(Kind::RpcResult, "hub", ());
        assert_eq!(answer.re, Some(question.id));
        assert_eq!(answer.conv, question.conv);
        assert_eq!(answer.to, "cli");
    }

    #[test]
    fn unknown_fields_and_kinds_are_tolerated() {
        let id = Uuid::new_v4();
        let frame = format!(
            r#"{{"v":1,"id":"{id}","kind":"teleport","from":"x","to":"hub","ts":1,"extra":true}}"#
        );
        let envelope = Envelope::decode(&frame).unwrap();
        assert_eq!(envelope.kind, Kind::Unknown);
        assert_eq!(envelope.body, Value::Null);
        assert!(!envelope.encode().contains("\"re\""));
    }

    #[test]
    fn bad_frames_are_rejected() {
        let id = Uuid::new_v4();
        let v2 = format!(r#"{{"v":2,"id":"{id}","kind":"rpc","from":"x","to":"hub","ts":1}}"#);
        assert!(matches!(
            Envelope::decode(&v2),
            Err(DecodeError::Version(2))
        ));
        assert!(matches!(
            Envelope::decode("{not json"),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            Envelope::decode(r#"{"v":1}"#),
            Err(DecodeError::Invalid(_))
        ));
        let huge = " ".repeat(MAX_FRAME + 1);
        assert!(matches!(
            Envelope::decode(&huge),
            Err(DecodeError::TooLarge(_))
        ));
    }
}
