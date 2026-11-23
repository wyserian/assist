//! Config loading and validation (PLAN.md §6).
//!
//! Only the sections the current phase uses are accepted; unknown keys are errors so typos
//! (`[contcts]`) never pass silently. Sections are added as their phases land.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::phrases::PhraseSet;
use crate::policy::PolicyOverrides;

/// The config format version this build reads.
pub const CURRENT_VERSION: u32 = 1;

/// The parsed `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Format version; must equal [`CURRENT_VERSION`].
    #[serde(default = "current_version")]
    pub config_version: u32,
    /// Apps by name; per-OS launch values are read by the open-app action.
    #[serde(default)]
    pub apps: BTreeMap<String, toml::Value>,
    /// Named scripts; values are read by the script action.
    #[serde(default)]
    pub scripts: BTreeMap<String, toml::Value>,
    /// Contacts by name, value is the phone number.
    #[serde(default)]
    pub contacts: BTreeMap<String, String>,
    /// Per-command settings, keyed by command name.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandConfig>,
    /// Overrides of the default policy table (PLAN.md §3.6).
    #[serde(default)]
    pub policy: PolicyOverrides,
}

/// Settings for one command (`[commands.<name>]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    /// Always ask for confirmation before running.
    #[serde(default)]
    pub risky: bool,
    /// Question to ask per missing slot, e.g. `ask.text = "What's the message?"`.
    #[serde(default)]
    pub ask: BTreeMap<String, String>,
}

/// Why a config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file that was read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The TOML is invalid or has unknown or wrongly typed keys.
    #[error("line {line}, column {column}: {message}")]
    Parse {
        /// 1-based line.
        line: usize,
        /// 1-based column.
        column: usize,
        /// The parser's message.
        message: String,
    },
    /// The file was written for another format version.
    #[error("unsupported config_version {found}; this build reads version {expected}")]
    Version {
        /// Version in the file.
        found: u32,
        /// Version this build reads.
        expected: u32,
    },
    /// A `[commands.<name>]` section names a command no phrase file defines.
    #[error("[commands.{0}] does not match any known command")]
    UnknownCommand(String),
}

fn current_version() -> u32 {
    CURRENT_VERSION
}

impl Config {
    /// Parses and validates config text. An empty text gives an empty, valid config.
    pub fn from_toml(src: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(src).map_err(|err| {
            let (line, column) = location(src, &err);
            ConfigError::Parse {
                line,
                column,
                message: err.message().to_owned(),
            }
        })?;
        if config.config_version != CURRENT_VERSION {
            return Err(ConfigError::Version {
                found: config.config_version,
                expected: CURRENT_VERSION,
            });
        }
        Ok(config)
    }

    /// Reads and parses a config file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&src)
    }

    /// Checks that every `[commands.<name>]` names a command defined in `phrases`, so a typo
    /// can never silently drop `risky = true`.
    pub fn validate_commands(&self, phrases: &PhraseSet) -> Result<(), ConfigError> {
        match self.commands.keys().find(|name| !phrases.contains(name)) {
            Some(name) => Err(ConfigError::UnknownCommand(name.clone())),
            None => Ok(()),
        }
    }

    /// Value lists that `list:<name>` slots match against: `apps`, `contacts`, `scripts`.
    pub fn lists(&self) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("apps".to_owned(), self.apps.keys().cloned().collect()),
            (
                "contacts".to_owned(),
                self.contacts.keys().cloned().collect(),
            ),
            ("scripts".to_owned(), self.scripts.keys().cloned().collect()),
        ])
    }
}

/// 1-based line and column of a TOML error in `src`.
pub(crate) fn location(src: &str, err: &toml::de::Error) -> (usize, usize) {
    let offset = err.span().map_or(0, |span| span.start);
    let before = src.get(..offset).unwrap_or(src);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_valid() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.config_version, CURRENT_VERSION);
        assert!(config.apps.is_empty());
        assert_eq!(config.lists().len(), 3);
    }

    #[test]
    fn unknown_section_reports_its_line() {
        let err = Config::from_toml("config_version = 1\n\n[contcts]\nmom = \"1\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 3, .. }), "{err}");
    }

    #[test]
    fn wrong_type_is_an_error() {
        let err = Config::from_toml("[commands.send_sms]\nrisky = \"yes\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 2, .. }), "{err}");
    }

    #[test]
    fn other_version_is_rejected() {
        let err = Config::from_toml("config_version = 2").unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Version {
                    found: 2,
                    expected: 1
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn misspelled_command_is_rejected() {
        let config = Config::from_toml("[commands.sned_sms]\nrisky = true\n").unwrap();
        let phrases = PhraseSet::default_en().unwrap();
        let err = config.validate_commands(&phrases).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownCommand(ref name) if name == "sned_sms"));
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = Config::load(Path::new("definitely/not/here.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "{err}");
    }
}
