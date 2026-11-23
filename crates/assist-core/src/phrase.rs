//! Phrase syntax (PLAN.md section 3.7): `a|b` alternatives, `( )` groups, `?` optional,
//! `.name.` slots. Words are separated by spaces.
//!
//! Parsed by hand: the grammar is a few rules, so a parser library would add more than it saves.

/// A parsed phrase. [`parse`] always returns a [`Node::Choice`] at the top level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A literal word, lowercased.
    Word(String),
    /// A captured slot, written `.name.`.
    Slot(String),
    /// Alternatives; each alternative is a sequence of nodes.
    Choice(Vec<Vec<Node>>),
    /// An optional node, written `x?`.
    Optional(Box<Node>),
}

/// A phrase syntax error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("column {column}: {message}")]
pub struct SyntaxError {
    /// 1-based column (in characters) where the error was found.
    pub column: usize,
    /// What went wrong.
    pub message: String,
}

/// Parses one phrase.
pub fn parse(src: &str) -> Result<Node, SyntaxError> {
    let mut parser = Parser { src, pos: 0 };
    let alternatives = parser.alternatives()?;
    match parser.peek() {
        None => Ok(Node::Choice(alternatives)),
        Some(c) => Err(parser.error(format!("unexpected '{c}'"))),
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.src
            .get(self.pos..)
            .and_then(|rest| rest.chars().next())
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn eat(&mut self, want: char) -> bool {
        let found = self.peek() == Some(want);
        if found {
            self.bump();
        }
        found
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(&keep) {
            self.bump();
        }
        self.src.get(start..self.pos).unwrap_or_default()
    }

    fn error(&self, message: impl Into<String>) -> SyntaxError {
        let before = self.src.get(..self.pos).unwrap_or_default();
        SyntaxError {
            column: before.chars().count() + 1,
            message: message.into(),
        }
    }

    fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, SyntaxError> {
        let mut alternatives = vec![self.sequence()?];
        while self.eat('|') {
            alternatives.push(self.sequence()?);
        }
        Ok(alternatives)
    }

    fn sequence(&mut self) -> Result<Vec<Node>, SyntaxError> {
        let mut sequence = Vec::new();
        loop {
            self.take_while(char::is_whitespace);
            match self.peek() {
                None | Some('|' | ')') => break,
                Some(_) => sequence.push(self.item()?),
            }
        }
        if sequence.is_empty() {
            return Err(self.error("empty alternative"));
        }
        Ok(sequence)
    }

    fn item(&mut self) -> Result<Node, SyntaxError> {
        let atom = self.atom()?;
        Ok(if self.eat('?') {
            Node::Optional(Box::new(atom))
        } else {
            atom
        })
    }

    fn atom(&mut self) -> Result<Node, SyntaxError> {
        match self.peek() {
            Some('(') => {
                self.bump();
                let alternatives = self.alternatives()?;
                if !self.eat(')') {
                    return Err(self.error("missing ')'"));
                }
                Ok(Node::Choice(alternatives))
            }
            Some('.') => {
                self.bump();
                let name = self.take_while(|c| c.is_ascii_alphanumeric() || c == '_');
                if name.is_empty() || !self.eat('.') {
                    return Err(self.error("slot must look like .name."));
                }
                Ok(Node::Slot(name.to_owned()))
            }
            Some(c) if is_word_char(c) => {
                Ok(Node::Word(self.take_while(is_word_char).to_lowercase()))
            }
            Some(c) => Err(self.error(format!("unexpected '{c}'"))),
            None => Err(self.error("unexpected end")),
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phrases() {
        insta::assert_debug_snapshot!("simple", parse("Open .app.").unwrap());
        insta::assert_debug_snapshot!(
            "groups",
            parse("(send (a)? message|text) (to)? .contact.").unwrap()
        );
        insta::assert_debug_snapshot!(
            "nested_optional",
            parse("text .contact. ((saying|that) .text.)?").unwrap()
        );
    }

    #[test]
    fn reports_errors_with_column() {
        let err = |src: &str| {
            let e = parse(src).unwrap_err();
            (e.column, e.message)
        };
        assert_eq!(err("(open"), (6, "missing ')'".to_owned()));
        assert_eq!(err("open|"), (6, "empty alternative".to_owned()));
        assert_eq!(err(".app"), (5, "slot must look like .name.".to_owned()));
        assert_eq!(err("open)"), (5, "unexpected ')'".to_owned()));
        assert_eq!(err("open #"), (6, "unexpected '#'".to_owned()));
        assert_eq!(err("x??"), (3, "unexpected '?'".to_owned()));
        assert_eq!(err(""), (1, "empty alternative".to_owned()));
    }
}
