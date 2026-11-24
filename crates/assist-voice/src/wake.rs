//! Wake word and "stop" with sherpa's open-vocabulary keyword spotter: any phrase works, so
//! the wake word follows the persona name in config.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use sherpa_onnx as so;

use crate::{VoiceError, RATE};

/// Marks the start of a word in the model's pieces.
const WORD_START: char = '\u{2581}';

/// Model files inside [`crate::KWS_MODEL`].
const FILES: [&str; 3] = ["encoder", "decoder", "joiner"];
const FILE_SUFFIX: &str = "-epoch-12-avg-2-chunk-16-left-64.onnx";

/// Boost and threshold per keyword, tuned on the Phase 2 spike: every wake phrase found and
/// no false alarm in 74 s of the owner's speech.
const WAKE_OPTIONS: &str = ":2.0 #0.15";
const STOP_OPTIONS: &str = ":1.0 #0.35";
const WAKE_LABEL: &str = "wake";
const STOP_LABEL: &str = "stop";

/// What was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    /// One of the wake phrases.
    Wake,
    /// "stop".
    Stop,
}

/// Listens for the wake phrases and "stop" in a continuous audio stream.
pub struct Spotter {
    inner: so::KeywordSpotter,
    stream: so::OnlineStream,
}

impl Spotter {
    /// Loads the model in `dir` and listens for `wake` phrases (e.g. "hey bee") and "stop".
    /// Fails if no wake phrase can be spelled with the model's pieces.
    pub fn new(dir: &Path, wake: &[String]) -> Result<Self, VoiceError> {
        let file = |name: &str| {
            let path = dir.join(name);
            if path.is_file() {
                Ok(Some(path.to_string_lossy().into_owned()))
            } else {
                Err(VoiceError::Model(path))
            }
        };
        let tokens = std::fs::read_to_string(dir.join("tokens.txt"))
            .map_err(|_| VoiceError::Model(dir.join("tokens.txt")))?;
        let mut config = so::KeywordSpotterConfig::default();
        config.model_config.transducer.encoder = file(&format!("{}{FILE_SUFFIX}", FILES[0]))?;
        config.model_config.transducer.decoder = file(&format!("{}{FILE_SUFFIX}", FILES[1]))?;
        config.model_config.transducer.joiner = file(&format!("{}{FILE_SUFFIX}", FILES[2]))?;
        config.model_config.tokens = file("tokens.txt")?;
        config.model_config.num_threads = 1;
        config.keywords_buf = Some(keywords(&tokens, wake)?);
        let inner =
            so::KeywordSpotter::create(&config).ok_or(VoiceError::Engine("keyword spotter"))?;
        let stream = inner.create_stream();
        Ok(Self { inner, stream })
    }

    /// Feeds 16 kHz audio; returns a keyword that ended in it.
    pub fn push(&mut self, samples: &[f32]) -> Option<Keyword> {
        self.stream.accept_waveform(RATE as i32, samples);
        let mut heard = None;
        while self.inner.is_ready(&self.stream) {
            self.inner.decode(&self.stream);
            let Some(result) = self.inner.get_result(&self.stream) else {
                continue;
            };
            let keyword = match result.keyword.as_str() {
                WAKE_LABEL => Keyword::Wake,
                STOP_LABEL => Keyword::Stop,
                _ => continue,
            };
            self.inner.reset(&self.stream);
            // The wake word wins over a "stop" in the same chunk.
            if heard != Some(Keyword::Wake) {
                heard = Some(keyword);
            }
        }
        heard
    }
}

/// The keyword list in the model's pieces: every spelling of each wake phrase, then "stop".
fn keywords(tokens: &str, wake: &[String]) -> Result<String, VoiceError> {
    let rank: HashMap<&str, usize> = tokens
        .lines()
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(token, id)| Some((token, id.parse().ok()?)))
        .collect();
    let known = |pieces: &Vec<String>| pieces.iter().all(|p| rank.contains_key(p.as_str()));
    let mut lines = Vec::new();
    for phrase in wake {
        let spellings: BTreeSet<Vec<String>> = spellings(&phrase.to_uppercase(), &rank)
            .into_iter()
            .filter(known)
            .collect();
        for pieces in spellings {
            lines.push(format!("{} {WAKE_OPTIONS} @{WAKE_LABEL}", pieces.join(" ")));
        }
    }
    if lines.is_empty() {
        return Err(VoiceError::Wake(wake.join(", ")));
    }
    let stop = bpe("STOP", &rank);
    if known(&stop) {
        lines.push(format!("{} {STOP_OPTIONS} @{STOP_LABEL}", stop.join(" ")));
    }
    Ok(lines.join("\n"))
}

/// How the model may spell a phrase. The model hears a short name loosely: "hey bee" comes
/// out as HEY BEE, HEY BE, HEYBE or HEY B, so the last word gets every such variant.
fn spellings(phrase: &str, rank: &HashMap<&str, usize>) -> Vec<Vec<String>> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let Some((last, first)) = words.split_last() else {
        return Vec::new();
    };
    let head: Vec<String> = first.iter().flat_map(|w| bpe(w, rank)).collect();
    let collapsed = collapse(last);
    let mut tails = vec![
        bpe(last, rank),
        letters(last, true),
        bpe(&collapsed, rank),
        letters(&collapsed, true),
    ];
    if !first.is_empty() {
        // Run into the word before: no word start.
        tails.push(letters(&collapsed, false));
    }
    // A name that sounds like a letter ("bee", "dee") is heard as that letter.
    if collapsed.len() == 2 && collapsed.ends_with('E') {
        tails.push(letters(&collapsed[..1], true));
    }
    tails
        .into_iter()
        .map(|tail| head.iter().cloned().chain(tail).collect())
        .collect()
}

/// sentencepiece BPE: start from letters, keep merging the adjacent pair whose joined piece
/// ranks best (lowest id in tokens.txt).
fn bpe(word: &str, rank: &HashMap<&str, usize>) -> Vec<String> {
    let mut pieces = letters(word, true);
    loop {
        let best = pieces
            .windows(2)
            .enumerate()
            .filter_map(|(i, pair)| {
                Some((*rank.get(format!("{}{}", pair[0], pair[1]).as_str())?, i))
            })
            .min();
        let Some((_, i)) = best else {
            return pieces;
        };
        let next = pieces.remove(i + 1);
        pieces[i].push_str(&next);
    }
}

/// One piece per letter, the first marked as a word start if `start`.
fn letters(word: &str, start: bool) -> Vec<String> {
    word.chars()
        .enumerate()
        .map(|(i, c)| {
            if i == 0 && start {
                format!("{WORD_START}{c}")
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Trailing repeated letters as one: BEE sounds like BE.
fn collapse(word: &str) -> String {
    let mut out = word.to_owned();
    while out.len() > 1 && out.ends_with(|c| Some(c) == out.chars().rev().nth(1)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few pieces of the real model, ranked as in its tokens.txt.
    const TOKENS: &str =
        "<blk> 0\nS 3\nT 4\nE 10\nY 17\nO 20\nP 25\nB 34\n\u{2581}BE 43\n\u{2581}HE 49\n\u{2581}B 68\n\u{2581}ST 93\n\u{2581}O 132\nK 140\nA 150\n";

    #[test]
    fn hey_bee_has_every_heard_spelling_and_stop() {
        let list = keywords(TOKENS, &["hey bee".to_owned()]).unwrap();
        let lines: Vec<&str> = list.lines().collect();
        for spelling in [
            "\u{2581}HE Y \u{2581}BE E",
            "\u{2581}HE Y \u{2581}B E E",
            "\u{2581}HE Y \u{2581}BE",
            "\u{2581}HE Y B E",
            "\u{2581}HE Y \u{2581}B",
        ] {
            let line = format!("{spelling} {WAKE_OPTIONS} @{WAKE_LABEL}");
            assert!(
                lines.contains(&line.as_str()),
                "missing {spelling} in {list}"
            );
        }
        assert_eq!(
            lines.last(),
            Some(&format!("\u{2581}ST O P {STOP_OPTIONS} @{STOP_LABEL}").as_str())
        );
    }

    #[test]
    fn a_phrase_the_model_cannot_spell_is_an_error() {
        assert!(keywords(TOKENS, &["hey 42".to_owned()]).is_err());
    }

    #[test]
    fn collapse_merges_trailing_repeats_only() {
        assert_eq!(collapse("BEE"), "BE");
        assert_eq!(collapse("BUMBLEBEE"), "BUMBLEBE");
        assert_eq!(collapse("ALEX"), "ALEX");
    }
}
