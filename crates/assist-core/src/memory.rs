//! Long-term memory (PLAN.md section 3.17): facts the owner asked to keep, such as
//! "remember that Sam's number is 1234". Kept in `<data>/memory.json` (user-only), never
//! sent anywhere, listed and deleted with `assist memory` and the dashboard. Secrets,
//! passwords, one-time codes and card numbers are refused: they belong in the keystore.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::normalize::normalize;

/// Most records kept.
pub const MAX_RECORDS: usize = 1000;
/// Longest text one record keeps, in bytes.
pub const MAX_TEXT: usize = 500;

/// Words that mark something that must not be kept as memory.
const SENSITIVE: [&str; 14] = [
    "password",
    "passcode",
    "passphrase",
    "pin code",
    "otp",
    "one time code",
    "one-time code",
    "verification code",
    "cvv",
    "card number",
    "api key",
    "secret",
    "token",
    "private key",
];

/// Serializes writes within the daemon, so two jobs cannot lose each other's change.
static LOCK: Mutex<()> = Mutex::new(());

/// One remembered fact; the wire type, so clients list the same records.
pub use assist_proto::rpc::MemoryRecord as Record;

fn path(data: &Path) -> PathBuf {
    data.join("memory.json")
}

/// Every record, oldest first; empty when there is no memory yet.
pub fn list(data: &Path) -> Vec<Record> {
    std::fs::read(path(data))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(data: &Path, records: &[Record]) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(records).map_err(|e| e.to_string())?;
    crate::private_fs::write_atomic(&path(data), &json)
        .map_err(|e| format!("cannot save memory: {e}"))
}

/// Splits "Sam's number is 1234" into subject and fact, or refuses what must not be kept.
pub fn check(text: &str) -> Result<(String, String), String> {
    let text = text.trim().trim_start_matches("that ").trim();
    if text.is_empty() {
        return Err("Remember what?".to_owned());
    }
    if text.len() > MAX_TEXT {
        return Err(format!(
            "That is too long to remember (at most {MAX_TEXT} characters)."
        ));
    }
    let lower = normalize(text);
    let sensitive = SENSITIVE.iter().any(|word| lower.contains(word)) || has_card_number(text);
    if sensitive {
        return Err(
            "I don't keep passwords, codes, keys or card numbers as memory; use assist secret set."
                .to_owned(),
        );
    }
    for copula in [" is ", " are ", " was "] {
        if let Some((subject, fact)) = text.split_once(copula) {
            if !subject.trim().is_empty() && !fact.trim().is_empty() {
                let fact = fact.trim().trim_end_matches('.').to_owned();
                return Ok((subject.trim().to_owned(), fact));
            }
        }
    }
    Ok((text.trim_end_matches('.').to_owned(), String::new()))
}

/// Adds a record; `now` is Unix ms.
pub fn add(data: &Path, text: &str, source: &str, now: u64) -> Result<Record, String> {
    let (subject, fact) = check(text)?;
    let _lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut records = list(data);
    if records.len() >= MAX_RECORDS {
        return Err(format!(
            "Memory is full ({MAX_RECORDS} records); forget some first."
        ));
    }
    // Saying it again replaces what was said before about the same subject.
    records.retain(|r| fact.is_empty() || normalize(&r.subject) != normalize(&subject));
    let id = records.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    let record = Record {
        id,
        subject,
        fact,
        source: source.to_owned(),
        created: now,
        last_used: now,
    };
    records.push(record.clone());
    save(data, &records)?;
    Ok(record)
}

/// The records that match `query` best, best first; "everything" matches all.
pub fn matching(data: &Path, query: &str) -> Vec<Record> {
    let records = list(data);
    let query = normalize(query);
    if query == "everything" || query == "all" {
        return records;
    }
    let words: Vec<&str> = query
        .split(' ')
        .filter(|w| w.len() > 1 && !QUESTION_WORDS.contains(w))
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, Record)> = records
        .into_iter()
        .filter_map(|r| {
            let text = normalize(&format!("{} {}", r.subject, r.fact));
            let hits = words
                .iter()
                .filter(|w| text.split(' ').any(|t| same_word(t, w)))
                .count();
            (hits * 2 >= words.len() && hits > 0).then_some((hits, r))
        })
        .collect();
    scored.sort_by_key(|(hits, _)| std::cmp::Reverse(*hits));
    scored.into_iter().map(|(_, r)| r).collect()
}

/// Words of a question that say nothing about what is asked for.
const QUESTION_WORDS: [&str; 18] = [
    "the", "my", "is", "a", "an", "of", "what", "about", "where", "did", "do", "does", "who",
    "when", "how", "you", "remember", "know",
];

/// The same word, or one a stem of the other ("park", "parked"); "sam's" is "sam".
fn same_word(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches("'s"), b.trim_end_matches("'s"));
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (short.len() >= 4 && long.starts_with(short))
}

/// The best record for `query`, marked as used now.
pub fn recall(data: &Path, query: &str, now: u64) -> Option<Record> {
    let best = matching(data, query).into_iter().next()?;
    let _lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut records = list(data);
    if let Some(record) = records.iter_mut().find(|r| r.id == best.id) {
        record.last_used = now;
    }
    // Losing a last-used time is harmless, so a failed save does not stop the answer.
    let _ = save(data, &records);
    Some(best)
}

/// Deletes the records with these ids; returns how many.
pub fn forget(data: &Path, ids: &[u64]) -> Result<usize, String> {
    let _lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut records = list(data);
    let before = records.len();
    records.retain(|r| !ids.contains(&r.id));
    save(data, &records)?;
    Ok(before - records.len())
}

/// A run of 13 to 19 digits (spaces and dashes allowed) that passes the card checksum.
fn has_card_number(text: &str) -> bool {
    let mut digits = Vec::new();
    for c in text.chars().chain(std::iter::once('x')) {
        match c {
            '0'..='9' => digits.push(c as u8 - b'0'),
            ' ' | '-' if !digits.is_empty() => {}
            _ => {
                if (13..=19).contains(&digits.len()) && luhn(&digits) {
                    return true;
                }
                digits.clear();
            }
        }
    }
    false
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn remembers_recalls_replaces_and_forgets() {
        let data = temp("main");
        let sam = add(&data, "that Sam's number is 98765 43210", "cli", 1).unwrap();
        assert_eq!(sam.sentence(), "Sam's number is 98765 43210.");
        add(&data, "I parked on level 3", "voice", 2).unwrap();
        assert_eq!(recall(&data, "what's sam's number", 5).unwrap().id, sam.id);
        assert_eq!(recall(&data, "where did I park", 6).unwrap().fact, "");
        assert!(recall(&data, "the weather", 7).is_none());
        add(&data, "Sam's number is 11111", "cli", 8).unwrap();
        assert_eq!(list(&data).len(), 2, "same subject replaced");
        assert_eq!(matching(&data, "everything").len(), 2);
        assert_eq!(
            matching(&data, "sam").len(),
            1,
            "possessive matches the name"
        );
        assert_eq!(forget(&data, &[sam.id + 2]).unwrap(), 1);
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn refuses_secrets_and_card_numbers() {
        for text in [
            "my wifi password is hunter2",
            "the OTP is 123456",
            "my card number is 4111 1111 1111 1111",
            "4111-1111-1111-1111 is the visa",
            "my github token is abc",
        ] {
            assert!(check(text).is_err(), "{text}");
        }
        assert!(check("mom's phone is +91 98765 43210").is_ok());
        assert!(check("").is_err());
    }
}
