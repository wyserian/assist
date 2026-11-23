//! Config loading and validation (PLAN.md section 6).
//!
//! Only the sections the current phase uses are accepted; unknown keys are errors so typos
//! (`[contcts]`) never pass silently.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::dialog::SHELL;
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
    /// Overrides of the default policy table (PLAN.md section 3.6).
    #[serde(default)]
    pub policy: PolicyOverrides,
    /// How many jobs may run at once per lane.
    #[serde(default)]
    pub lanes: LanesConfig,
    /// Job limits.
    #[serde(default)]
    pub jobs: JobsConfig,
    /// Web search.
    #[serde(default)]
    pub search: SearchConfig,
    /// The local daemon.
    #[serde(default)]
    pub server: ServerConfig,
}

/// `[server]`: where `assist serve` listens.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// A loopback address and port; other interfaces are refused.
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 7720)),
        }
    }
}

/// `[search]`: the search engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SearchConfig {
    /// An `http(s)://` URL with `{query}` where the encoded query goes.
    pub url: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            url: "https://duckduckgo.com/?q={query}".to_owned(),
        }
    }
}

/// `[lanes]`: concurrent jobs per lane (PLAN.md section 3.8).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LanesConfig {
    /// Quick, user-facing jobs.
    pub interactive: usize,
    /// Long jobs: builds, scans, scrapes.
    pub background: usize,
}

impl Default for LanesConfig {
    fn default() -> Self {
        Self {
            interactive: 2,
            background: 3,
        }
    }
}

/// `[jobs]`: per-job limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// A running job is stopped after this long, e.g. `"10m"`.
    #[serde(deserialize_with = "duration")]
    pub timeout: Duration,
    /// Output kept in memory per job for `jobs` listings; older lines are dropped.
    pub output_tail_kb: usize,
    /// Finished jobs and their logs are deleted after this many days.
    pub retention_days: u64,
    /// The oldest job logs are deleted while all logs together exceed this size.
    pub log_cap_mb: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            output_tail_kb: 64,
            retention_days: 7,
            log_cap_mb: 500,
        }
    }
}

/// Parses `30s`, `10m` or `2h`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (digits, unit) = [('s', 1), ('m', 60), ('h', 3600)]
        .into_iter()
        .find_map(|(suffix, secs)| text.strip_suffix(suffix).map(|d| (d, secs)))?;
    let n: u64 = digits.parse().ok()?;
    Some(Duration::from_secs(n.checked_mul(unit)?))
}

fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_duration(&text).ok_or_else(|| {
        D::Error::custom(format!("invalid duration {text:?}; use e.g. 30s, 10m, 2h"))
    })
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
    /// A value is well-formed but not allowed.
    #[error("{0}")]
    Invalid(String),
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
        if config.lanes.interactive == 0 || config.lanes.background == 0 {
            return Err(ConfigError::Invalid(
                "[lanes] values must be at least 1".to_owned(),
            ));
        }
        // Only web URLs: the URL is handed to the OS "open" handler.
        let url = &config.search.url;
        let web = url.starts_with("https://") || url.starts_with("http://");
        if !web || !url.contains("{query}") {
            return Err(ConfigError::Invalid(
                "[search] url must start with https:// or http:// and contain {query}".to_owned(),
            ));
        }
        if !config.server.bind.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "[server] bind must be a loopback address such as 127.0.0.1:7720".to_owned(),
            ));
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

    /// Checks that every `[commands.<name>]` names a command defined in `phrases` (or the
    /// built-in `shell`), so a typo can never silently drop `risky = true`.
    pub fn validate_commands(&self, phrases: &PhraseSet) -> Result<(), ConfigError> {
        let unknown = |name: &&String| !phrases.contains(name) && name.as_str() != SHELL;
        match self.commands.keys().find(unknown) {
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
    fn job_limits_parse_and_validate() {
        let config =
            Config::from_toml("[lanes]\ninteractive = 1\n[jobs]\ntimeout = \"90s\"\n").unwrap();
        assert_eq!(config.lanes.interactive, 1);
        assert_eq!(config.lanes.background, 3);
        assert_eq!(config.jobs.timeout, Duration::from_secs(90));
        assert!(matches!(
            Config::from_toml("[lanes]\nbackground = 0\n"),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            Config::from_toml("[jobs]\ntimeout = \"soon\"\n"),
            Err(ConfigError::Parse { line: 2, .. })
        ));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("10"), None);
    }

    #[test]
    fn server_binds_to_loopback_only() {
        assert_eq!(Config::from_toml("").unwrap().server.bind.port(), 7720);
        assert!(Config::from_toml(
            "[server]
bind = \"[::1]:8000\"
"
        )
        .is_ok());
        assert!(matches!(
            Config::from_toml(
                "[server]
bind = \"0.0.0.0:7720\"
"
            ),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn search_url_must_be_a_web_url_with_a_query() {
        assert!(Config::from_toml("[search]\nurl = \"https://x.org/?q={query}\"\n").is_ok());
        for bad in [
            "file:///c:/x?{query}",
            "https://x.org/",
            "javascript:{query}",
        ] {
            let src = format!("[search]\nurl = \"{bad}\"\n");
            assert!(
                matches!(Config::from_toml(&src), Err(ConfigError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = Config::load(Path::new("definitely/not/here.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "{err}");
    }
}
