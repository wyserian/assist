//! What the assistant says: templates from `config/phrases/en/replies.toml`, overridable in
//! config `[replies]`, filled with the `[persona]` name and address (PLAN.md section 3.17).

use std::collections::BTreeMap;

use crate::config::Config;

/// The embedded English lines.
const DEFAULT_EN: &str = include_str!("../../../config/phrases/en/replies.toml");

/// A line the dialog can say; each has a key in the replies file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// When a voice client starts listening.
    Greeting,
    /// The wake word alone: the assistant waits for the command.
    Wake,
    /// Input matched nothing.
    NotUnderstood,
    /// Unmatched input offered to the shell; `{text}`.
    OfferShell,
    /// A question timed out.
    Expired,
    /// "cancel" stopped a question or queue.
    Cancelled,
    /// "cancel" with nothing waiting.
    NothingToCancel,
    /// "$" alone.
    EmptyShell,
    /// The command is not available here.
    Unavailable,
    /// Policy denies the command; `{source}`.
    NotAllowed,
    /// Confirmation for a command without its own `confirm`; `{action}`.
    Confirm,
    /// A command without its own `reply` started.
    Started,
    /// A number slot got something else.
    SayNumber,
    /// The answer is not one of the choices; `{text}`.
    UnknownChoice,
    /// "no" to a confirmation.
    Declined,
    /// Neither yes nor no.
    YesOrNo,
    /// A correction that fits no slot; `{text}`.
    UnclearCorrection,
    /// Several matches; `{options}`.
    DidYouMean,
    /// Input on a conversation another client owns; `{source}`.
    OtherOwner,
}

impl Line {
    /// Every line, for validation.
    pub const ALL: [Line; 19] = [
        Line::Greeting,
        Line::Wake,
        Line::NotUnderstood,
        Line::OfferShell,
        Line::Expired,
        Line::Cancelled,
        Line::NothingToCancel,
        Line::EmptyShell,
        Line::Unavailable,
        Line::NotAllowed,
        Line::Confirm,
        Line::Started,
        Line::SayNumber,
        Line::UnknownChoice,
        Line::Declined,
        Line::YesOrNo,
        Line::UnclearCorrection,
        Line::DidYouMean,
        Line::OtherOwner,
    ];

    /// The key in the replies file.
    pub fn key(self) -> &'static str {
        match self {
            Line::Greeting => "greeting",
            Line::Wake => "wake",
            Line::NotUnderstood => "not_understood",
            Line::OfferShell => "offer_shell",
            Line::Expired => "expired",
            Line::Cancelled => "cancelled",
            Line::NothingToCancel => "nothing_to_cancel",
            Line::EmptyShell => "empty_shell",
            Line::Unavailable => "unavailable",
            Line::NotAllowed => "not_allowed",
            Line::Confirm => "confirm",
            Line::Started => "started",
            Line::SayNumber => "say_number",
            Line::UnknownChoice => "unknown_choice",
            Line::Declined => "declined",
            Line::YesOrNo => "yes_or_no",
            Line::UnclearCorrection => "unclear_correction",
            Line::DidYouMean => "did_you_mean",
            Line::OtherOwner => "other_owner",
        }
    }

    /// The line with this key, if any.
    pub fn from_key(key: &str) -> Option<Line> {
        Line::ALL.into_iter().find(|line| line.key() == key)
    }
}

/// The reply templates with the persona filled in.
#[derive(Debug, Clone)]
pub struct Replies {
    lines: BTreeMap<&'static str, String>,
    name: String,
    address: String,
}

impl Replies {
    /// The default lines with `config`'s overrides and persona. Config validation has
    /// already rejected unknown `[replies]` keys.
    pub fn new(config: &Config) -> Self {
        // The embedded file is checked by a test, so a parse failure here cannot happen in
        // a released build; missing lines then fall back to their key.
        let defaults: BTreeMap<String, String> = toml::from_str(DEFAULT_EN).unwrap_or_default();
        let lines = Line::ALL
            .into_iter()
            .map(|line| {
                let key = line.key();
                let text = config
                    .replies
                    .get(key)
                    .or_else(|| defaults.get(key))
                    .cloned()
                    .unwrap_or_else(|| key.to_owned());
                (key, text)
            })
            .collect();
        Self {
            lines,
            name: config.persona.name.clone(),
            address: config.persona.address.clone(),
        }
    }

    /// One of the built-in lines.
    pub fn line(&self, line: Line, vars: &[(&str, &str)]) -> String {
        let template = self.lines.get(line.key()).map_or("", String::as_str);
        self.fill(template, vars)
    }

    /// Any template, e.g. a command's `reply`, with `vars` plus `{name}` and `{address}`.
    pub fn fill(&self, template: &str, vars: &[(&str, &str)]) -> String {
        let persona = [
            ("name", self.name.as_str()),
            ("address", self.address.as_str()),
        ];
        render(template, |key| {
            vars.iter()
                .chain(&persona)
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
        })
    }
}

/// Replaces `{key}` with its value. An empty value also removes the spaces and comma before
/// it; an unknown key stays as written so a typo shows.
fn render<'a>(template: &str, value: impl Fn(&str) -> Option<&'a str>) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        let placeholder = &rest[open..=open + len];
        match value(&placeholder[1..len]) {
            Some("") => {
                let kept = out.trim_end_matches(' ').trim_end_matches(',').len();
                out.truncate(kept);
            }
            Some(v) => out.push_str(v),
            None => out.push_str(placeholder),
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_has_exactly_the_known_lines() {
        let defaults: BTreeMap<String, String> = toml::from_str(DEFAULT_EN).unwrap();
        for line in Line::ALL {
            assert!(defaults.contains_key(line.key()), "missing {}", line.key());
        }
        for key in defaults.keys() {
            assert!(Line::from_key(key).is_some(), "unknown {key}");
        }
    }

    #[test]
    fn persona_fills_lines_and_overrides_apply() {
        let config = Config::from_toml(
            "[persona]\nname = \"Bee\"\naddress = \"Prime\"\n[replies]\ncancelled = \"Stopped, {address}.\"\n",
        )
        .unwrap();
        let replies = Replies::new(&config);
        assert_eq!(
            replies.line(Line::Greeting, &[]),
            "Hello Prime. Bee is listening."
        );
        assert_eq!(replies.line(Line::Cancelled, &[]), "Stopped, Prime.");
        assert_eq!(
            replies.fill("Opening {app}.", &[("app", "firefox")]),
            "Opening firefox."
        );
    }

    #[test]
    fn empty_address_takes_its_separator_along() {
        let config = Config::from_toml("[persona]\naddress = \"\"\n").unwrap();
        let replies = Replies::new(&config);
        assert_eq!(
            replies.line(Line::NotUnderstood, &[]),
            "Sorry, please say that again."
        );
        assert_eq!(
            replies.line(Line::Greeting, &[]),
            "Hello. Bee is listening."
        );
        assert_eq!(replies.fill("Done, {address}.", &[]), "Done.");
    }

    #[test]
    fn unknown_placeholders_stay_visible() {
        let replies = Replies::new(&Config::from_toml("").unwrap());
        assert_eq!(replies.fill("Hi {nmae} {", &[]), "Hi {nmae} {");
    }
}
