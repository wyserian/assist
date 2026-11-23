//! Text -> [`Intent`] (PLAN.md section 3.7).
//!
//! [`RuleMatcher`] compiles every phrase to an anchored regex. A [`RegexSet`] finds the
//! candidate phrases in one pass; slots are then resolved by type. When several phrases match,
//! the one that explains the most input words literally (not via slots) wins; ties go to the
//! command that sorts first.

use std::collections::BTreeMap;

use regex::{Regex, RegexSet};

use crate::config::Config;
use crate::normalize::{normalize, parse_number};
use crate::phrase::{self, Node, SyntaxError};
use crate::phrases::PhraseSet;

/// Minimum similarity (0 to 1) for a fuzzy `list:` slot match, e.g. "firfox" -> "firefox".
const FUZZY_MIN: f64 = 0.8;
/// Similarity when the input is the leading or trailing whole words of a value ("sam" -> "sam lee").
const PARTIAL_SCORE: f64 = 0.9;
/// Candidates this close to the best one count as ambiguous ("sam lee" vs "sam roy").
const AMBIGUITY_MARGIN: f64 = 0.05;

/// A recognised command with its resolved slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Command name, e.g. `send_sms`.
    pub command: String,
    /// Slot name -> value. `list:` slots hold the config key (the best candidate if
    /// ambiguous), `number` slots digits.
    pub slots: BTreeMap<String, String>,
    /// `list:` slots with several equally good candidates, best first; the dialog asks.
    pub ambiguous: BTreeMap<String, Vec<String>>,
}

/// Anything that turns text into an intent. Semantic and LLM matchers implement it too, so
/// callers never change.
pub trait Matcher: Send + Sync {
    /// The best intent for `text`, or `None` if nothing matches.
    fn best(&self, text: &str) -> Option<Intent>;
}

/// The type of a slot, declared in the phrase file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotType {
    /// Free text.
    Text,
    /// A whole number, digits or words.
    Number,
    /// One of the keys of a config list: `apps`, `contacts` or `scripts`.
    List(String),
}

/// Why phrases could not be compiled.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// A phrase has a syntax error.
    #[error("{command}: phrase {phrase:?}: {source}")]
    Syntax {
        /// Command the phrase belongs to.
        command: String,
        /// The phrase text.
        phrase: String,
        /// The syntax error.
        source: SyntaxError,
    },
    /// A phrase uses `.slot.` without declaring it in `slots`.
    #[error("{command}: slot .{slot}. is used but not declared in slots")]
    UndeclaredSlot {
        /// Command the phrase belongs to.
        command: String,
        /// The slot name.
        slot: String,
    },
    /// A slot type is not `text`, `number` or `list:<name>`.
    #[error("{command}: slot {slot} has unknown type {ty:?} (use text, number or list:<name>)")]
    UnknownSlotType {
        /// Command the slot belongs to.
        command: String,
        /// The slot name.
        slot: String,
        /// The type as written.
        ty: String,
    },
    /// A `list:<name>` slot names a list the config does not provide.
    #[error("{command}: slot {slot} uses unknown list {list:?} (known: apps, contacts, scripts)")]
    UnknownList {
        /// Command the slot belongs to.
        command: String,
        /// The slot name.
        slot: String,
        /// The list name as written.
        list: String,
    },
    /// The generated regex failed to compile (a bug, or a pathological phrase set).
    #[error("cannot compile phrases: {0}")]
    Regex(#[from] regex::Error),
}

#[derive(Debug)]
struct CompiledPhrase {
    command: String,
    regex: Regex,
    /// Capture group `i + 1` holds `slots[i]`.
    slots: Vec<(String, SlotType)>,
}

/// Slots resolved from one phrase match.
struct Extracted {
    slots: BTreeMap<String, String>,
    ambiguous: BTreeMap<String, Vec<String>>,
    /// Input words consumed by slots.
    captured: usize,
}

/// The deterministic rule matcher built from phrase files and config lists.
#[derive(Debug)]
pub struct RuleMatcher {
    set: RegexSet,
    phrases: Vec<CompiledPhrase>,
    /// Command -> slot -> type.
    types: BTreeMap<String, BTreeMap<String, SlotType>>,
    /// List name -> (normalized value, config key).
    lists: BTreeMap<String, Vec<(String, String)>>,
}

impl RuleMatcher {
    /// Compiles all phrases; `config` provides the values of `list:` slots.
    pub fn new(phrases: &PhraseSet, config: &Config) -> Result<Self, BuildError> {
        let lists: BTreeMap<String, Vec<(String, String)>> = config
            .lists()
            .into_iter()
            .map(|(name, keys)| (name, keys.into_iter().map(|k| (normalize(&k), k)).collect()))
            .collect();

        let mut compiled = Vec::new();
        let mut types = BTreeMap::new();
        for (command, spec) in phrases.commands() {
            let kinds = slot_types(command, &spec.slots, &lists)?;
            for text in &spec.phrases {
                let node = phrase::parse(text).map_err(|source| BuildError::Syntax {
                    command: command.to_owned(),
                    phrase: text.clone(),
                    source,
                })?;
                let mut pattern = String::from("^");
                let mut slots = Vec::new();
                emit(&node, &kinds, &mut pattern, &mut slots).map_err(|slot| {
                    BuildError::UndeclaredSlot {
                        command: command.to_owned(),
                        slot,
                    }
                })?;
                pattern.push('$');
                compiled.push(CompiledPhrase {
                    command: command.to_owned(),
                    regex: Regex::new(&pattern)?,
                    slots,
                });
            }
            types.insert(command.to_owned(), kinds);
        }
        let set = RegexSet::new(compiled.iter().map(|p| p.regex.as_str()))?;
        Ok(Self {
            set,
            phrases: compiled,
            types,
            lists,
        })
    }

    /// The declared slots of a command and their types.
    pub fn slots(&self, command: &str) -> Option<&BTreeMap<String, SlotType>> {
        self.types.get(command)
    }

    /// Keys of config list `list` that match normalized `value`, best first. Several are
    /// returned only when they are about equally close; an exact match is never ambiguous.
    pub fn candidates(&self, list: &str, value: &str) -> Vec<String> {
        let Some(entries) = self.lists.get(list) else {
            return Vec::new();
        };
        let mut scored: Vec<(f64, &String)> = entries
            .iter()
            .map(|(normalized, key)| (similarity(normalized, value), key))
            .filter(|(score, _)| *score >= FUZZY_MIN)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let Some(&(top, _)) = scored.first() else {
            return Vec::new();
        };
        let margin = if top >= 1.0 { 0.0 } else { AMBIGUITY_MARGIN };
        scored
            .into_iter()
            .take_while(|(score, _)| *score >= top - margin)
            .map(|(_, key)| key.clone())
            .collect()
    }

    /// Resolved slots, or `None` if a slot value is not valid for its type.
    fn extract(&self, phrase: &CompiledPhrase, input: &str) -> Option<Extracted> {
        let caps = phrase.regex.captures(input)?;
        let mut out = Extracted {
            slots: BTreeMap::new(),
            ambiguous: BTreeMap::new(),
            captured: 0,
        };
        for (i, (name, ty)) in phrase.slots.iter().enumerate() {
            let Some(m) = caps.get(i + 1) else { continue };
            // The same slot can appear in several alternatives; the first that matched wins.
            if out.slots.contains_key(name) {
                continue;
            }
            let value = m.as_str();
            out.captured += value.split(' ').count();
            let resolved = match ty {
                SlotType::Text => value.to_owned(),
                SlotType::Number => parse_number(value)?.to_string(),
                SlotType::List(list) => {
                    let candidates = self.candidates(list, value);
                    let best = candidates.first()?.clone();
                    if candidates.len() > 1 {
                        out.ambiguous.insert(name.clone(), candidates);
                    }
                    best
                }
            };
            out.slots.insert(name.clone(), resolved);
        }
        Some(out)
    }
}

impl Matcher for RuleMatcher {
    fn best(&self, text: &str) -> Option<Intent> {
        let normalized = normalize(text);
        if normalized.is_empty() {
            return None;
        }
        // Every compiled word and slot starts with a space, so the input does too.
        let input = format!(" {normalized}");
        let words = normalized.split(' ').count();

        let mut best: Option<(usize, Intent)> = None;
        for index in self.set.matches(&input).into_iter() {
            let phrase = &self.phrases[index];
            let Some(found) = self.extract(phrase, &input) else {
                continue;
            };
            let literal = words.saturating_sub(found.captured);
            let better = match &best {
                Some((top, _)) => literal > *top,
                None => true,
            };
            if better {
                let intent = Intent {
                    command: phrase.command.clone(),
                    slots: found.slots,
                    ambiguous: found.ambiguous,
                };
                best = Some((literal, intent));
            }
        }
        best.map(|(_, intent)| intent)
    }
}

/// How close normalized `value` is to normalized list `entry`, 0 to 1.
fn similarity(entry: &str, value: &str) -> f64 {
    if entry == value {
        return 1.0;
    }
    let leading = entry
        .strip_prefix(value)
        .is_some_and(|rest| rest.starts_with(' '));
    let trailing = entry
        .strip_suffix(value)
        .is_some_and(|rest| rest.ends_with(' '));
    if leading || trailing {
        return PARTIAL_SCORE;
    }
    strsim::normalized_levenshtein(entry, value)
}

fn slot_types(
    command: &str,
    declared: &BTreeMap<String, String>,
    lists: &BTreeMap<String, Vec<(String, String)>>,
) -> Result<BTreeMap<String, SlotType>, BuildError> {
    declared
        .iter()
        .map(|(slot, ty)| {
            let kind = match (ty.as_str(), ty.strip_prefix("list:")) {
                ("text", _) => SlotType::Text,
                ("number", _) => SlotType::Number,
                (_, Some(list)) if lists.contains_key(list) => SlotType::List(list.to_owned()),
                (_, Some(list)) => {
                    return Err(BuildError::UnknownList {
                        command: command.to_owned(),
                        slot: slot.clone(),
                        list: list.to_owned(),
                    })
                }
                (_, None) => {
                    return Err(BuildError::UnknownSlotType {
                        command: command.to_owned(),
                        slot: slot.clone(),
                        ty: ty.clone(),
                    })
                }
            };
            Ok((slot.clone(), kind))
        })
        .collect()
}

/// Appends the regex for `node`; returns the slot name if it is not declared.
fn emit(
    node: &Node,
    types: &BTreeMap<String, SlotType>,
    out: &mut String,
    slots: &mut Vec<(String, SlotType)>,
) -> Result<(), String> {
    match node {
        Node::Word(word) => {
            out.push(' ');
            out.push_str(&regex::escape(word));
        }
        Node::Slot(name) => {
            let ty = types.get(name).ok_or_else(|| name.clone())?;
            out.push_str(" (.+?)");
            slots.push((name.clone(), ty.clone()));
        }
        Node::Choice(alternatives) => {
            out.push_str("(?:");
            for (i, sequence) in alternatives.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                for node in sequence {
                    emit(node, types, out, slots)?;
                }
            }
            out.push(')');
        }
        Node::Optional(inner) => {
            out.push_str("(?:");
            emit(inner, types, out, slots)?;
            out.push_str(")?");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str =
        "[apps]\nfirefox = {}\n[contacts]\n\"sam lee\" = \"1\"\n\"sam roy\" = \"2\"\n";

    fn build(phrases: &str) -> Result<RuleMatcher, BuildError> {
        let config = Config::from_toml(CONFIG).unwrap();
        RuleMatcher::new(
            &PhraseSet::from_toml("test.toml", phrases).unwrap(),
            &config,
        )
    }

    #[test]
    fn undeclared_slot_is_an_error() {
        let err = build("[a]\nphrases = [\"open .app.\"]").unwrap_err();
        assert!(matches!(err, BuildError::UndeclaredSlot { ref slot, .. } if slot == "app"));
    }

    #[test]
    fn unknown_list_is_an_error() {
        let err = build("[a]\nslots = { app = \"list:planets\" }\nphrases = [\"open .app.\"]")
            .unwrap_err();
        assert!(matches!(err, BuildError::UnknownList { ref list, .. } if list == "planets"));
    }

    #[test]
    fn unknown_slot_type_is_an_error() {
        let err =
            build("[a]\nslots = { app = \"colour\" }\nphrases = [\"open .app.\"]").unwrap_err();
        assert!(matches!(err, BuildError::UnknownSlotType { .. }));
    }

    #[test]
    fn syntax_error_names_the_command() {
        let err = build("[a]\nphrases = [\"(open\"]").unwrap_err();
        assert!(matches!(err, BuildError::Syntax { ref command, .. } if command == "a"));
    }

    #[test]
    fn same_slot_in_two_alternatives() {
        let matcher =
            build("[a]\nslots = { app = \"list:apps\" }\nphrases = [\"(open .app.|.app. now)\"]")
                .unwrap();
        let intent = matcher.best("Firefox now").unwrap();
        assert_eq!(intent.slots["app"], "firefox");
    }

    #[test]
    fn empty_input_matches_nothing() {
        let matcher = build("[a]\nphrases = [\"hello\"]").unwrap();
        assert_eq!(matcher.best("?!"), None);
    }

    #[test]
    fn partial_names_are_ambiguous_exact_ones_are_not() {
        let matcher =
            build("[call]\nslots = { who = \"list:contacts\" }\nphrases = [\"call .who.\"]")
                .unwrap();
        let intent = matcher.best("call sam").unwrap();
        assert_eq!(intent.ambiguous["who"], ["sam lee", "sam roy"]);
        let intent = matcher.best("call roy").unwrap();
        assert_eq!(intent.slots["who"], "sam roy");
        assert!(intent.ambiguous.is_empty());
        assert_eq!(matcher.candidates("contacts", "sam lee"), ["sam lee"]);
        assert!(matcher.candidates("contacts", "bob").is_empty());
    }
}
