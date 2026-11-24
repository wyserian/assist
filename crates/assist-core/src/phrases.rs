//! Phrase files (`config/phrases/<lang>/*.toml`): commands, their slot types and phrases.
//!
//! ```toml
//! [send_sms]
//! slots   = { contact = "list:contacts", text = "text" }
//! phrases = ["(send (a)? message|text) (to)? .contact. (saying .text.)?"]
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table};

use crate::config::location;

/// Default English phrases, embedded so the binary works with no files on disk.
const DEFAULT_EN: [(&str, &str); 6] = [
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
        "screen.toml",
        include_str!("../../../config/phrases/en/screen.toml"),
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
    /// Slot name -> type: `text`, `number` or `list:<name>`.
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    /// Phrases in the syntax of [`crate::phrase`].
    pub phrases: Vec<String>,
    /// Question per slot to ask when it is missing; config `ask` overrides these.
    #[serde(default)]
    pub ask: BTreeMap<String, String>,
    /// What to say when the command starts, e.g. "Opening {app}." (slot values filled in).
    pub reply: Option<String>,
    /// The confirmation question, e.g. "Call {contact}?"; slot values are shown as in the
    /// readback (text quoted, contacts with their number).
    pub confirm: Option<String>,
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

    /// One command's spec.
    pub fn spec(&self, command: &str) -> Option<&CommandSpec> {
        self.commands.get(command)
    }

    /// Every `*.toml` file in `dir`, in name order; empty if the directory does not exist.
    /// Blocking.
    pub fn load_dir(dir: &Path) -> Result<Self, PhrasesError> {
        let io_error = |file: &Path, err: io::Error| PhrasesError {
            file: file.display().to_string(),
            line: 0,
            column: 0,
            message: err.to_string(),
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(io_error(dir, err)),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();
        let mut set = Self::default();
        for path in files {
            let src = std::fs::read_to_string(&path).map_err(|err| io_error(&path, err))?;
            set.extend(Self::from_toml(&path.display().to_string(), &src)?);
        }
        Ok(set)
    }

    /// The same set with `phrase` added to `command`.
    pub fn with_phrase(&self, command: &str, phrase: &str) -> Option<Self> {
        let mut set = self.clone();
        let spec = set.commands.get_mut(command)?;
        if !spec.phrases.iter().any(|p| p == phrase) {
            spec.phrases.push(phrase.to_owned());
        }
        Some(set)
    }
}

/// Adds `phrase` for `command` to the user phrase file `file`, keeping its comments. A
/// command that is not in the file yet is copied in from `spec` first, because a user file
/// entry replaces the built-in one. Blocking.
pub fn add_user_phrase(
    file: &Path,
    command: &str,
    spec: &CommandSpec,
    phrase: &str,
) -> io::Result<()> {
    let src = match std::fs::read_to_string(file) {
        Ok(src) => src,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let mut doc: DocumentMut = src.parse().map_err(io::Error::other)?;
    if !doc.contains_table(command) {
        let mut table = Table::new();
        let mut slots = InlineTable::new();
        for (slot, ty) in &spec.slots {
            slots.insert(slot, ty.as_str().into());
        }
        let mut ask = InlineTable::new();
        for (slot, question) in &spec.ask {
            ask.insert(slot, question.as_str().into());
        }
        table.insert("slots", value(slots));
        if !ask.is_empty() {
            table.insert("ask", value(ask));
        }
        table.insert(
            "phrases",
            value(Array::from_iter(spec.phrases.iter().map(String::as_str))),
        );
        doc.insert(command, Item::Table(table));
    }
    let phrases = doc[command]["phrases"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("[{command}] phrases is not a list")))?;
    if !phrases.iter().any(|p| p.as_str() == Some(phrase)) {
        phrases.push(phrase);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_phrases_are_added_to_a_copy_and_reload() {
        let dir = std::env::temp_dir().join(format!("assist-phrases-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("user.toml");
        let defaults = PhraseSet::default_en().unwrap();
        let spec = defaults.spec("open_app").unwrap();

        add_user_phrase(&file, "open_app", spec, "fire up .app.").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("# my phrases\n{text}")).unwrap();
        add_user_phrase(&file, "open_app", spec, "fire up the .app.").unwrap();
        add_user_phrase(&file, "open_app", spec, "fire up .app.").unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("# my phrases"),
            "comments are kept:\n{text}"
        );
        let user = PhraseSet::load_dir(&dir).unwrap();
        let phrases = &user.spec("open_app").unwrap().phrases;
        assert_eq!(
            phrases.len(),
            spec.phrases.len() + 2,
            "built-ins kept, no duplicate"
        );
        assert_eq!(user.spec("open_app").unwrap().slots, spec.slots);
        assert!(PhraseSet::load_dir(&dir.join("missing"))
            .unwrap()
            .spec("x")
            .is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
