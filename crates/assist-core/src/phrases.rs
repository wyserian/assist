//! Phrase files (`config/phrases/<lang>/*.toml`): commands, their slot types and phrases.
//!
//! ```toml
//! [send_sms]
//! slots   = { contact = "list:contacts", text = "text" }
//! phrases = ["(send (a)? message|text) (to)? .contact. (saying .text.)?"]
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::location;

/// Default English phrases, embedded so the binary works with no files on disk.
const DEFAULT_EN: [(&str, &str); 5] = [
    (
        "apps.toml",
        include_str!("../../../config/phrases/en/apps.toml"),
    ),
    (
        "media.toml",
        include_str!("../../../config/phrases/en/media.toml"),
    ),
    (
        "messaging.toml",
        include_str!("../../../config/phrases/en/messaging.toml"),
    ),
    (
        "scripts.toml",
        include_str!("../../../config/phrases/en/scripts.toml"),
    ),
    (
        "search.toml",
        include_str!("../../../config/phrases/en/search.toml"),
    ),
];

/// One command in a phrase file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Slot name → type: `text`, `number` or `list:<name>`.
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    /// Phrases in the syntax of [`crate::phrase`].
    pub phrases: Vec<String>,
}

/// All known commands, keyed by name (sorted, so matching is deterministic).
#[derive(Debug, Clone, Default)]
pub struct PhraseSet {
    commands: BTreeMap<String, CommandSpec>,
}

/// A phrase file that is not valid TOML or has unknown keys.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{file}: line {line}, column {column}: {message}")]
pub struct PhrasesError {
    /// File name, for the message.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub column: usize,
    /// The parser's message.
    pub message: String,
}

impl PhraseSet {
    /// Parses one phrase file; `file` is only used in error messages.
    pub fn from_toml(file: &str, src: &str) -> Result<Self, PhrasesError> {
        let commands = toml::from_str(src).map_err(|err| {
            let (line, column) = location(src, &err);
            PhrasesError {
                file: file.to_owned(),
                line,
                column,
                message: err.message().to_owned(),
            }
        })?;
        Ok(Self { commands })
    }

    /// The embedded English defaults.
    pub fn default_en() -> Result<Self, PhrasesError> {
        let mut set = Self::default();
        for (file, src) in DEFAULT_EN {
            set.extend(Self::from_toml(file, src)?);
        }
        Ok(set)
    }

    /// Adds `other`; a command defined in both is replaced by `other`'s (user overrides).
    pub fn extend(&mut self, other: PhraseSet) {
        self.commands.extend(other.commands);
    }

    /// Whether a command with this name exists.
    pub fn contains(&self, command: &str) -> bool {
        self.commands.contains_key(command)
    }

    /// Commands in name order.
    pub fn commands(&self) -> impl Iterator<Item = (&str, &CommandSpec)> {
        self.commands
            .iter()
            .map(|(name, spec)| (name.as_str(), spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load() {
        let set = PhraseSet::default_en().unwrap();
        assert!(set.contains("open_app"));
        assert!(set.contains("send_sms"));
    }

    #[test]
    fn user_file_overrides_a_command() {
        let mut set = PhraseSet::default_en().unwrap();
        set.extend(PhraseSet::from_toml("mine.toml", "[open_app]\nphrases = [\"go\"]").unwrap());
        let (_, spec) = set
            .commands()
            .find(|(name, _)| *name == "open_app")
            .unwrap();
        assert_eq!(spec.phrases, ["go"]);
    }

    #[test]
    fn unknown_key_reports_file_and_line() {
        let err = PhraseSet::from_toml("bad.toml", "[x]\nphrases = []\nphrase = []\n").unwrap_err();
        assert_eq!((err.file.as_str(), err.line), ("bad.toml", 3), "{err}");
    }
}
