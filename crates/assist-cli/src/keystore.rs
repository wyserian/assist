//! The OS keystore as a [`SecretStore`]: Windows Credential Manager, macOS Keychain or the
//! Secret Service (GNOME Keyring, KWallet) on Linux. Entries are named `assist / <name>`.

use assist_core::secrets::{SecretError, SecretStore};
use keyring::{Entry, Error};

const SERVICE: &str = "assist";

/// The platform keystore.
pub struct OsKeystore;

impl OsKeystore {
    /// The keystore, or why it cannot be used on this machine.
    pub fn open() -> Result<Self, SecretError> {
        Entry::store_status()
            .as_ref()
            .map(|()| Self)
            .map_err(|err| SecretError::Store(err.to_string()))
    }
}

fn entry(name: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, name).map_err(store_error)
}

fn store_error(err: Error) -> SecretError {
    SecretError::Store(err.to_string())
}

impl SecretStore for OsKeystore {
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        entry(name)?.set_password(value).map_err(store_error)
    }

    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(Error::NoEntry) => Ok(None),
            Err(err) => Err(store_error(err)),
        }
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match entry(name)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(err) => Err(store_error(err)),
        }
    }
}
