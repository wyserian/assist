//! API keys and other secrets (PLAN.md section 3.14).
//!
//! Values live in a [`SecretStore`] (the OS keystore on desktop) and are referred to by name.
//! Nothing outside this module can read a value back: [`Secrets`] offers set, delete and a
//! metadata listing. Known values are scrubbed by the [`Redactor`] before job output, titles
//! or audit records are stored or shown.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use assist_proto::now_ms;
use serde::{Deserialize, Serialize};

use crate::private_fs;

/// Longest accepted secret value.
pub const MAX_VALUE: usize = 16 * 1024;

/// Values shorter than this are stored but not redacted, so short common strings in output
/// are never mangled.
pub const MIN_REDACTED: usize = 8;

/// Where secret values are kept. Implementations are blocking.
pub trait SecretStore: Send + Sync {
    /// Stores or replaces a value.
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError>;
    /// Reads a value; `None` if there is none.
    fn get(&self, name: &str) -> Result<Option<String>, SecretError>;
    /// Removes a value; succeeds if there was none.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

/// Why a secret operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The name is empty, too long or has characters other than `a-z 0-9 _ -`.
    #[error("invalid secret name {0:?}: use 1 to 64 of a-z, 0-9, _ and -")]
    InvalidName(String),
    /// The value is empty or longer than [`MAX_VALUE`].
    #[error("secret value must be 1 to {MAX_VALUE} bytes")]
    InvalidValue,
    /// The OS keystore failed.
    #[error("keystore: {0}")]
    Store(String),
    /// The metadata file failed.
    #[error("secret metadata: {0}")]
    Io(#[from] io::Error),
}

/// What is known about a secret without its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretInfo {
    /// When it was set, Unix ms.
    pub set: u64,
    /// The last 4 characters, to tell keys apart.
    pub last4: String,
}

/// The secrets of this device: write-only values, readable metadata.
pub struct Secrets {
    store: Box<dyn SecretStore>,
    meta_path: PathBuf,
    meta: Mutex<BTreeMap<String, SecretInfo>>,
    redactor: Redactor,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("names", &self.list().keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Secrets {
    /// Opens the secrets listed in `dir/secrets.json`, loading their values for redaction.
    /// Blocking. Names whose value disappeared from the store are dropped from the list.
    pub fn open(store: Box<dyn SecretStore>, dir: &Path) -> Result<Self, SecretError> {
        private_fs::create_dir(dir)?;
        let meta_path = dir.join("secrets.json");
        let mut meta: BTreeMap<String, SecretInfo> = match std::fs::read(&meta_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::from)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => return Err(err.into()),
        };
        let redactor = Redactor::default();
        let mut missing = Vec::new();
        for name in meta.keys() {
            match store.get(name)? {
                Some(value) => redactor.add(name, &value),
                None => missing.push(name.clone()),
            }
        }
        for name in &missing {
            meta.remove(name);
        }
        let secrets = Self {
            store,
            meta_path,
            meta: Mutex::new(meta),
            redactor,
        };
        if !missing.is_empty() {
            secrets.save(&secrets.lock())?;
        }
        Ok(secrets)
    }

    /// Stores or replaces a secret. Blocking.
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        if value.is_empty() || value.len() > MAX_VALUE {
            return Err(SecretError::InvalidValue);
        }
        self.store.set(name, value)?;
        let last4: String = {
            let chars: Vec<char> = value.chars().collect();
            chars[chars.len().saturating_sub(4)..].iter().collect()
        };
        let mut meta = self.lock();
        meta.insert(
            name.to_owned(),
            SecretInfo {
                set: now_ms(),
                last4,
            },
        );
        self.save(&meta)?;
        self.redactor.add(name, value);
        Ok(())
    }

    /// Removes a secret; false if there was none. Blocking.
    pub fn delete(&self, name: &str) -> Result<bool, SecretError> {
        validate_name(name)?;
        let mut meta = self.lock();
        if !meta.contains_key(name) {
            return Ok(false);
        }
        self.store.delete(name)?;
        meta.remove(name);
        self.save(&meta)?;
        self.redactor.remove(name);
        Ok(true)
    }

    /// The value of secret `name`, for an adapter on this device (the Telegram bot's token).
    /// Never sent over the wire. Blocking.
    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        validate_name(name)?;
        if !self.lock().contains_key(name) {
            return Ok(None);
        }
        self.store.get(name)
    }

    /// Every secret's metadata, by name. Never contains values.
    pub fn list(&self) -> BTreeMap<String, SecretInfo> {
        self.lock().clone()
    }

    /// The redactor that knows every current value.
    pub fn redactor(&self) -> Redactor {
        self.redactor.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, SecretInfo>> {
        self.meta.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes the metadata atomically.
    fn save(&self, meta: &BTreeMap<String, SecretInfo>) -> Result<(), SecretError> {
        let json = serde_json::to_vec_pretty(meta).map_err(io::Error::from)?;
        private_fs::write_atomic(&self.meta_path, &json)?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), SecretError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(SecretError::InvalidName(name.to_owned()))
    }
}

/// Replaces known secret values with `[secret:name]`. Cheap to clone; clones share values.
#[derive(Clone, Default)]
pub struct Redactor {
    values: Arc<RwLock<BTreeMap<String, String>>>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Redactor")
    }
}

impl Redactor {
    /// `text` with every known value replaced. Borrows when nothing matched.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let values = self.values.read().unwrap_or_else(PoisonError::into_inner);
        // Longest first, so a value that contains another one is replaced whole.
        let mut ordered: Vec<(&String, &String)> = values.iter().collect();
        ordered.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
        let mut out = Cow::Borrowed(text);
        for (name, value) in ordered {
            if out.contains(value.as_str()) {
                out = Cow::Owned(out.replace(value.as_str(), &format!("[secret:{name}]")));
            }
        }
        out
    }

    pub(crate) fn add(&self, name: &str, value: &str) {
        let mut values = self.values.write().unwrap_or_else(PoisonError::into_inner);
        if value.len() >= MIN_REDACTED {
            values.insert(name.to_owned(), value.to_owned());
        } else {
            values.remove(name);
        }
    }

    fn remove(&self, name: &str) {
        self.values
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }
}

/// Compares in time that depends only on the length, not on where the inputs differ, for
/// tokens and pairing secrets.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[derive(Default, Clone)]
    struct Memory(Arc<Mutex<BTreeMap<String, String>>>);

    impl SecretStore for Memory {
        fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
            Ok(self.0.lock().unwrap().get(name).cloned())
        }
        fn delete(&self, name: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("assist-secrets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn set_list_redact_delete_and_reopen() {
        let dir = temp_dir("cycle");
        let memory = Memory::default();
        let secrets = Secrets::open(Box::new(memory.clone()), &dir).unwrap();
        secrets.set("openai", "sk-live-1234567890abcd").unwrap();
        secrets.set("pin", "1234").unwrap();

        let list = secrets.list();
        assert_eq!(list["openai"].last4, "abcd");
        let shown = format!("{list:?}");
        assert!(!shown.contains("sk-live"), "metadata must not hold values");

        let redactor = secrets.redactor();
        assert_eq!(
            redactor.redact("key=sk-live-1234567890abcd ok"),
            "key=[secret:openai] ok"
        );
        assert_eq!(redactor.redact("pin 1234"), "pin 1234");
        assert!(matches!(redactor.redact("clean"), Cow::Borrowed(_)));

        let reopened = Secrets::open(Box::new(memory.clone()), &dir).unwrap();
        assert_eq!(reopened.list().len(), 2);
        assert_eq!(
            reopened.redactor().redact("sk-live-1234567890abcd"),
            "[secret:openai]"
        );

        assert!(reopened.delete("openai").unwrap());
        assert!(!reopened.delete("openai").unwrap());
        assert_eq!(
            reopened.redactor().redact("sk-live-1234567890abcd"),
            "sk-live-1234567890abcd"
        );
        assert!(memory.get("openai").unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_and_values_are_validated() {
        let dir = temp_dir("validate");
        let secrets = Secrets::open(Box::new(Memory::default()), &dir).unwrap();
        for bad in ["", "Upper", "a b", "../x", &"x".repeat(65)] {
            assert!(matches!(
                secrets.set(bad, "value"),
                Err(SecretError::InvalidName(_))
            ));
        }
        assert!(matches!(
            secrets.set("ok", ""),
            Err(SecretError::InvalidValue)
        ));
        assert!(matches!(
            secrets.set("ok", &"x".repeat(MAX_VALUE + 1)),
            Err(SecretError::InvalidValue)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_missing_from_the_store_are_dropped() {
        let dir = temp_dir("missing");
        let memory = Memory::default();
        let secrets = Secrets::open(Box::new(memory.clone()), &dir).unwrap();
        secrets.set("gone", "long-enough-value").unwrap();
        memory.delete("gone").unwrap();
        let reopened = Secrets::open(Box::new(memory), &dir).unwrap();
        assert!(reopened.list().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
