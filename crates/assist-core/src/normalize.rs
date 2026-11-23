//! Text normalization shared by phrases and user input.

/// Lowercases, turns punctuation into spaces and collapses whitespace.
///
/// Letters, digits and inner apostrophes are kept, so "What's up?" becomes "what's up".
/// Hyphens split words ("twenty-five" becomes "twenty five").
pub fn normalize(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len());
    for c in text.chars() {
        let c = if c == '\u{2019}' { '\'' } else { c };
        if c.is_alphanumeric() || c == '\'' {
            spaced.extend(c.to_lowercase());
        } else {
            spaced.push(' ');
        }
    }
    spaced
        .split_whitespace()
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

const UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Parses a non-negative whole number from digits or English words in normalized text.
///
/// shortcut: words cover 0–99 and "(a|one) hundred"; extend when a command needs larger
/// numbers or decimals.
pub fn parse_number(text: &str) -> Option<u32> {
    if let Ok(n) = text.parse::<u32>() {
        return Some(n);
    }
    let words: Vec<&str> = text.split(' ').collect();
    match words.as_slice() {
        ["hundred"] | ["a" | "one", "hundred"] => Some(100),
        [word] => unit(word).or_else(|| ten(word)),
        [tens, units] => Some(ten(tens)? + unit(units).filter(|u| (1..10).contains(u))?),
        _ => None,
    }
}

fn unit(word: &str) -> Option<u32> {
    UNITS
        .iter()
        .position(|w| *w == word)
        .and_then(|i| u32::try_from(i).ok())
}

fn ten(word: &str) -> Option<u32> {
    TENS.iter()
        .position(|w| *w == word)
        .and_then(|i| u32::try_from(i).ok())
        .map(|i| (i + 2) * 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_punctuation_and_spaces() {
        assert_eq!(normalize("  Launch   Firefox!! "), "launch firefox");
        assert_eq!(normalize("What\u{2019}s up?"), "what's up");
        assert_eq!(normalize("'quoted' twenty-five"), "quoted twenty five");
        assert_eq!(normalize("75%"), "75");
        assert_eq!(normalize("?!"), "");
    }

    #[test]
    fn parses_numbers() {
        assert_eq!(parse_number("50"), Some(50));
        assert_eq!(parse_number("zero"), Some(0));
        assert_eq!(parse_number("fifteen"), Some(15));
        assert_eq!(parse_number("fifty"), Some(50));
        assert_eq!(parse_number("twenty five"), Some(25));
        assert_eq!(parse_number("a hundred"), Some(100));
        assert_eq!(parse_number("twenty zero"), None);
        assert_eq!(parse_number("loud"), None);
        assert_eq!(parse_number(""), None);
    }
}
