//! [`crate::Rpc`] method names and their parameter and result types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Active and recent jobs. Result: `Vec<JobSummary>`.
pub const JOBS_LIST: &str = "jobs.list";
/// Older jobs from the store. Params: [`History`]. Result: `Vec<JobSummary>`.
pub const JOBS_HISTORY: &str = "jobs.history";
/// A job's last output lines. Params: [`JobRef`]. Result: `Vec<String>`.
pub const JOBS_TAIL: &str = "jobs.tail";
/// Stops a job. Params: [`JobRef`]. Result: `bool` (false if it had already ended).
pub const JOBS_STOP: &str = "jobs.stop";
/// Stops every job. Result: `usize`, how many.
pub const JOBS_STOP_ALL: &str = "jobs.stop_all";
/// Secret metadata. Result: `Vec<SecretEntry>`.
pub const SECRETS_LIST: &str = "secrets.list";
/// Stores or replaces a secret. Params: [`SecretSet`].
pub const SECRETS_SET: &str = "secrets.set";
/// Deletes a secret. Params: [`SecretName`]. Result: `bool`.
pub const SECRETS_DELETE: &str = "secrets.delete";
/// What a sentence would do, without doing it. Params: [`Explain`]. Result:
/// `Option<Explanation>`.
pub const COMMANDS_EXPLAIN: &str = "commands.explain";
/// Connected clients. Result: `Vec<ClientEntry>`.
pub const CLIENTS_LIST: &str = "clients.list";
/// Disconnects a client and ends its browser session. Params: [`ClientRef`]. Result: `bool`.
pub const CLIENTS_REVOKE: &str = "clients.revoke";
/// A one-time code for signing a browser in (token clients only). Result: [`Otp`].
pub const AUTH_OTP: &str = "auth.otp";
/// The newest audit records. Params: [`AuditPage`]. Result: `Vec<AuditEntry>`, oldest first.
pub const AUDIT_TAIL: &str = "audit.tail";

/// How many audit records to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditPage {
    /// At most this many (default 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One audit record as clients see it (without the hash chain fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unix ms.
    pub ts: u64,
    /// `submitted`, `running`, `done`, `failed`, ...
    pub event: String,
    /// Job id.
    pub job: u64,
    /// Command name.
    pub command: String,
    /// The resolved action, secrets redacted.
    pub title: String,
    /// Adapter, e.g. `cli`.
    pub source: String,
    /// Trust level.
    pub trust: String,
    /// Error message for failed jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A job id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRef {
    /// Job id.
    pub id: u64,
}

/// A page of job history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct History {
    /// At most this many jobs (default 50).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only jobs with a smaller id.
    #[serde(default)]
    pub before: Option<u64>,
}

/// A secret to store.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSet {
    /// Name, `a-z 0-9 _ -`.
    pub name: String,
    /// The value; never sent back.
    pub value: String,
}

impl std::fmt::Debug for SecretSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretSet")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A secret name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretName {
    /// Name.
    pub name: String,
}

/// One secret without its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntry {
    /// Name.
    pub name: String,
    /// When it was set, Unix ms.
    pub set: u64,
    /// The last 4 characters.
    pub last4: String,
}

/// A sentence to explain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explain {
    /// The sentence.
    pub text: String,
}

/// What a sentence would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    /// Matched command.
    pub command: String,
    /// Resolved slots.
    pub slots: BTreeMap<String, String>,
    /// Slots with several equally good values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ambiguous: BTreeMap<String, Vec<String>>,
    /// The action as it would be read back.
    pub readback: String,
    /// `allow`, `confirm` or `deny` for the asking client; `unavailable` if this device
    /// cannot run the command.
    pub decision: String,
}

/// A connected client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEntry {
    /// Connection id.
    pub id: u64,
    /// `cli` (token) or `web` (browser session).
    pub kind: String,
    /// Program name from its hello.
    pub client: String,
    /// When it connected, Unix ms.
    pub connected: u64,
}

/// A client id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRef {
    /// Connection id.
    pub id: u64,
}

/// A one-time sign-in code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Otp {
    /// The code; valid once, for a short time.
    pub code: String,
}

impl std::fmt::Debug for Otp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Otp(..)")
    }
}
