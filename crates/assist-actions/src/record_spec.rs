//! What `record` asks for: `screen|audio [for <length>] [to <file>]`. Shared by the desktop
//! action (ffmpeg) and the phone's, so both read a request the same way.

use std::time::Duration;

use assist_core::config::parse_duration;

/// Longest recording `for` accepts.
pub const MAX_FOR: Duration = Duration::from_secs(4 * 3600);

/// What to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The screen.
    Screen,
    /// The microphone.
    Audio,
}

/// A parsed `record` request.
#[derive(Debug, PartialEq, Eq)]
pub struct Spec {
    /// Screen or microphone.
    pub kind: Kind,
    /// How long; `None` runs until the job is stopped.
    pub length: Option<Duration>,
    /// Where to, as the user wrote it.
    pub file: Option<String>,
}

impl Spec {
    /// `screen|audio [for <length>] [to <file>]`; the file keeps its spelling.
    pub fn parse(text: &str) -> Result<Self, String> {
        let usage = || "use: record screen|audio [for 30s] [to file]".to_owned();
        let (head, file) = match text.split_once(" to ") {
            Some((head, file)) if !file.trim().is_empty() => (head, Some(file.trim().to_owned())),
            _ => (text, None),
        };
        let words: Vec<String> = head.split_whitespace().map(str::to_lowercase).collect();
        let kind = words
            .iter()
            .find_map(|w| match w.as_str() {
                "screen" | "desktop" => Some(Kind::Screen),
                "audio" | "mic" | "microphone" | "sound" | "voice" => Some(Kind::Audio),
                _ => None,
            })
            .ok_or_else(usage)?;
        let length = match words.iter().position(|w| w == "for") {
            Some(at) => Some(length(&words[at + 1..]).ok_or_else(usage)?),
            None => None,
        };
        if length.is_some_and(|l| l.is_zero() || l > MAX_FOR) {
            return Err("record for at most 4h".to_owned());
        }
        Ok(Self { kind, length, file })
    }
}

/// `30s`, `30 seconds`, `2 min`, `1 hour`.
fn length(words: &[String]) -> Option<Duration> {
    let joined = words.join(" ");
    if let Some(d) = parse_duration(&joined) {
        return Some(d);
    }
    let (n, unit) = (words.first()?, words.get(1)?);
    let unit = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => "s",
        "m" | "min" | "mins" | "minute" | "minutes" => "m",
        "h" | "hour" | "hours" => "h",
        _ => return None,
    };
    parse_duration(&format!("{n}{unit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_how_long_and_where() {
        let s = Spec::parse("screen for 30s to Demo Video.mkv").unwrap();
        assert_eq!(
            s,
            Spec {
                kind: Kind::Screen,
                length: Some(Duration::from_secs(30)),
                file: Some("Demo Video.mkv".to_owned())
            }
        );
        let s = Spec::parse("the microphone for 2 minutes").unwrap();
        assert_eq!(
            (s.kind, s.length),
            (Kind::Audio, Some(Duration::from_secs(120)))
        );
        assert_eq!(Spec::parse("audio").unwrap().length, None);
        assert!(Spec::parse("the garden").is_err());
        assert!(Spec::parse("screen for ever").is_err());
        assert!(Spec::parse("screen for 9h").is_err());
    }
}
