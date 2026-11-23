//! Append-only audit log (PLAN.md section 9): one JSON object per line in `audit/YYYY-MM.jsonl`.
//!
//! Each line carries `prev` (the previous line's hash) and `hash` (SHA-256 of the line without
//! `hash`), so editing or deleting a line breaks the chain and [`verify`] finds it.
//!
//! shortcut: each month file starts its own chain, so deleting a whole month file is not
//! detected; anchor each file to the previous one when logs are exported or synced.
//! shortcut: secret values in titles are not redacted; redact them once secrets are stored.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::private_fs;

/// `prev` of the first line in a file.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One audited event.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRecord {
    /// Unix time in milliseconds.
    pub ts: u64,
    /// What happened: `submitted`, `running`, `done`, `failed`, `cancelled`, `timed_out`.
    pub event: String,
    /// Job id.
    pub job: u64,
    /// Command name.
    pub command: String,
    /// The resolved action, as read back to the user.
    pub title: String,
    /// Adapter, e.g. `cli`.
    pub source: String,
    /// User as the adapter knows them.
    pub user: String,
    /// Trust level.
    pub trust: String,
    /// Error message for failed jobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Writes audit records to `dir`. Cheap to clone; all clones share one chain.
#[derive(Debug, Clone)]
pub struct Audit {
    inner: Arc<Mutex<Writer>>,
}

#[derive(Debug)]
struct Writer {
    dir: PathBuf,
    month: String,
    file: Option<File>,
    prev: String,
    last_error: Option<String>,
}

impl Audit {
    /// Opens (or creates) the audit directory.
    pub fn open(dir: &Path) -> io::Result<Self> {
        private_fs::create_dir(dir)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Writer {
                dir: dir.to_owned(),
                month: String::new(),
                file: None,
                prev: GENESIS.to_owned(),
                last_error: None,
            })),
        })
    }

    /// Appends a record on a blocking thread, so the async runtime is never blocked.
    /// Failures are kept in [`Audit::last_error`].
    pub fn record(&self, record: AuditRecord) {
        let audit = self.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(err) = audit.append(&record) {
                audit.lock().last_error = Some(err.to_string());
            }
        });
    }

    /// Appends a record now (blocking).
    pub fn append(&self, record: &AuditRecord) -> io::Result<()> {
        let mut writer = self.lock();
        let month = month_of(record.ts);
        if writer.file.is_none() || writer.month != month {
            let path = writer.dir.join(format!("{month}.jsonl"));
            writer.prev = last_hash(&path)?.unwrap_or_else(|| GENESIS.to_owned());
            writer.file = Some(private_fs::open_append(&path)?);
            writer.month = month;
        }
        let mut value = serde_json::to_value(record)?;
        let Value::Object(fields) = &mut value else {
            return Err(io::Error::other("audit record is not an object"));
        };
        fields.insert("prev".to_owned(), Value::String(writer.prev.clone()));
        let hash = hash_of(fields)?;
        fields.insert("hash".to_owned(), Value::String(hash.clone()));
        let line = serde_json::to_string(&value)?;
        if let Some(file) = writer.file.as_mut() {
            writeln!(file, "{line}")?;
            file.flush()?;
        }
        writer.prev = hash;
        Ok(())
    }

    /// The last write error, if any write failed.
    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Writer> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why an audit file does not verify.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The file could not be read.
    #[error("cannot read audit file: {0}")]
    Io(#[from] io::Error),
    /// A line is changed, missing or out of order.
    #[error("line {0}: chain broken")]
    Broken(usize),
}

/// Checks the hash chain of one audit file; returns the number of records.
pub fn verify(path: &Path) -> Result<usize, VerifyError> {
    let mut prev = GENESIS.to_owned();
    let mut count = 0;
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        let broken = || VerifyError::Broken(index + 1);
        let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(&line) else {
            return Err(broken());
        };
        let Some(Value::String(hash)) = fields.remove("hash") else {
            return Err(broken());
        };
        let linked = fields.get("prev").and_then(Value::as_str) == Some(prev.as_str());
        if !linked || hash_of(&fields).ok().as_deref() != Some(hash.as_str()) {
            return Err(broken());
        }
        prev = hash;
        count += 1;
    }
    Ok(count)
}

pub use assist_proto::now_ms;

fn hash_of(fields: &Map<String, Value>) -> io::Result<String> {
    // serde_json maps are sorted, so the serialization is canonical.
    let body = serde_json::to_string(fields)?;
    Ok(Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn last_hash(path: &Path) -> io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut last = None;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&line) {
            if let Some(Value::String(hash)) = fields.get("hash") {
                last = Some(hash.clone());
            }
        }
    }
    Ok(last)
}

/// `YYYY-MM` (UTC) of a Unix time in milliseconds.
fn month_of(ms: u64) -> String {
    let days = i64::try_from(ms / 86_400_000).unwrap_or(0);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn record(job: u64, event: &str) -> AuditRecord {
        AuditRecord {
            ts: 1_700_000_000_000,
            event: event.to_owned(),
            job,
            command: "shell".to_owned(),
            title: "$ ls".to_owned(),
            source: "cli".to_owned(),
            user: "owner".to_owned(),
            trust: "local".to_owned(),
            error: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn months_are_utc_calendar_months() {
        assert_eq!(month_of(0), "1970-01");
        assert_eq!(month_of(1_700_000_000_000), "2023-11");
        assert_eq!(month_of(951_782_400_000), "2000-02"); // 2000-02-29
    }

    #[test]
    fn chain_verifies_and_survives_reopen() {
        let dir = temp_dir("chain");
        let audit = Audit::open(&dir).unwrap();
        audit.append(&record(1, "submitted")).unwrap();
        audit.append(&record(1, "done")).unwrap();
        let reopened = Audit::open(&dir).unwrap();
        reopened.append(&record(2, "submitted")).unwrap();
        assert_eq!(verify(&dir.join("2023-11.jsonl")).unwrap(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_line_breaks_the_chain() {
        let dir = temp_dir("tamper");
        let audit = Audit::open(&dir).unwrap();
        for job in 1..=3 {
            audit.append(&record(job, "submitted")).unwrap();
        }
        let path = dir.join("2023-11.jsonl");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replacen("\"job\":2", "\"job\":9", 1)).unwrap();
        assert!(matches!(verify(&path), Err(VerifyError::Broken(2))));
        fs::remove_dir_all(&dir).unwrap();
    }
}
