//! Conditions over facts, such as `node.battery < 20 and not node.charging`: the `if` of
//! saved pipes, and later fact triggers and proactive rules (PLAN.md sections 3.11, 3.12).
//!
//! Grammar: `or` of `and` of `not`; a comparison `a == b`, `!=`, `<`, `<=`, `>`, `>=`, or a
//! single value; values are fact names, numbers, `'text'` and `true` / `false`;
//! parentheses group. A fact that is not set makes its comparison false.

use crate::facts::Value;

/// Longest condition accepted, in bytes; also bounds the nesting depth.
const MAX_LEN: usize = 512;

/// A parsed condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition(Expr);

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare(Operand, Op, Operand),
    Is(Operand),
}

#[derive(Debug, Clone, PartialEq)]
enum Operand {
    Fact(String),
    Literal(Value),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Why a condition does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("condition {text:?}: {reason}")]
pub struct ConditionError {
    /// The condition as written.
    pub text: String,
    /// What is wrong.
    pub reason: String,
}

impl Condition {
    /// Parses a condition.
    pub fn parse(text: &str) -> Result<Self, ConditionError> {
        let error = |reason: &str| ConditionError {
            text: text.to_owned(),
            reason: reason.to_owned(),
        };
        if text.len() > MAX_LEN {
            return Err(error("too long"));
        }
        let tokens = tokenize(text).map_err(|reason| error(&reason))?;
        let mut parser = Parser { tokens, at: 0 };
        let expr = parser.or().map_err(|reason| error(&reason))?;
        if parser.at < parser.tokens.len() {
            return Err(error("unexpected text after the condition"));
        }
        Ok(Self(expr))
    }

    /// Evaluates the condition, reading facts with `fact`.
    pub fn eval(&self, fact: &impl Fn(&str) -> Option<Value>) -> bool {
        eval(&self.0, fact)
    }
}

fn eval(expr: &Expr, fact: &impl Fn(&str) -> Option<Value>) -> bool {
    match expr {
        Expr::Or(a, b) => eval(a, fact) || eval(b, fact),
        Expr::And(a, b) => eval(a, fact) && eval(b, fact),
        Expr::Not(a) => !eval(a, fact),
        Expr::Is(a) => value(a, fact).is_some_and(|v| v.is_truthy()),
        Expr::Compare(a, op, b) => match (value(a, fact), value(b, fact)) {
            (Some(a), Some(b)) => compare(&a, *op, &b),
            _ => false,
        },
    }
}

fn value(operand: &Operand, fact: &impl Fn(&str) -> Option<Value>) -> Option<Value> {
    match operand {
        Operand::Fact(name) => fact(name),
        Operand::Literal(value) => Some(value.clone()),
    }
}

/// Numbers compare as numbers, text and booleans by equality (text also by order);
/// mixed kinds are never equal. Text compares case-insensitively, as names on screen vary.
fn compare(a: &Value, op: Op, b: &Value) -> bool {
    let ordering = match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Text(b)) => Some(a.to_lowercase().cmp(&b.to_lowercase())),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    };
    let Some(ordering) = ordering else {
        return op == Op::Ne;
    };
    match op {
        Op::Eq => ordering.is_eq(),
        Op::Ne => ordering.is_ne(),
        Op::Lt => ordering.is_lt(),
        Op::Le => ordering.is_le(),
        Op::Gt => ordering.is_gt(),
        Op::Ge => ordering.is_ge(),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Number(f64),
    Text(String),
    Op(Op),
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '(' || c == ')' {
            chars.next();
            tokens.push(if c == '(' { Token::Open } else { Token::Close });
        } else if c == '\'' || c == '"' {
            chars.next();
            let mut value = String::new();
            loop {
                match chars.next() {
                    Some((_, q)) if q == c => break,
                    Some((_, ch)) => value.push(ch),
                    None => return Err("text is not closed".to_owned()),
                }
            }
            tokens.push(Token::Text(value));
        } else if "=!<>".contains(c) {
            chars.next();
            let double = chars.peek().is_some_and(|&(_, next)| next == '=');
            if double {
                chars.next();
            }
            let op = match (c, double) {
                ('=', true) => Op::Eq,
                ('!', true) => Op::Ne,
                ('<', false) => Op::Lt,
                ('<', true) => Op::Le,
                ('>', false) => Op::Gt,
                ('>', true) => Op::Ge,
                _ => return Err(format!("unknown operator at {start}; use == != < <= > >=")),
            };
            tokens.push(Token::Op(op));
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            let mut end = start;
            while let Some(&(i, ch)) = chars.peek() {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-' {
                    end = i + ch.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &text[start..end];
            tokens.push(match word.parse::<f64>() {
                Ok(n) if n.is_finite() => Token::Number(n),
                _ => Token::Word(word.to_owned()),
            });
        } else {
            return Err(format!("unexpected {c:?}"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn keyword(&mut self, word: &str) -> bool {
        let found = matches!(self.peek(), Some(Token::Word(w)) if w.eq_ignore_ascii_case(word));
        if found {
            self.at += 1;
        }
        found
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.keyword("or") {
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.not()?;
        while self.keyword("and") {
            left = Expr::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.keyword("not") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        if self.peek() == Some(&Token::Open) {
            self.at += 1;
            let inner = self.or()?;
            if self.peek() != Some(&Token::Close) {
                return Err("missing )".to_owned());
            }
            self.at += 1;
            return Ok(inner);
        }
        let left = self.operand()?;
        if let Some(Token::Op(op)) = self.peek().cloned() {
            self.at += 1;
            let right = self.operand()?;
            return Ok(Expr::Compare(left, op, right));
        }
        Ok(Expr::Is(left))
    }

    fn operand(&mut self) -> Result<Operand, String> {
        let token = self.peek().cloned().ok_or("the condition ends too early")?;
        self.at += 1;
        Ok(match token {
            Token::Number(n) => Operand::Literal(Value::Number(n)),
            Token::Text(t) => Operand::Literal(Value::Text(t)),
            Token::Word(w) if w.eq_ignore_ascii_case("true") => Operand::Literal(Value::Bool(true)),
            Token::Word(w) if w.eq_ignore_ascii_case("false") => {
                Operand::Literal(Value::Bool(false))
            }
            Token::Word(w) if ["and", "or", "not"].contains(&w.to_lowercase().as_str()) => {
                return Err(format!("expected a value before {w:?}"));
            }
            Token::Word(w) => Operand::Fact(w),
            other => return Err(format!("expected a value, found {other:?}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn check(text: &str) -> bool {
        let facts = BTreeMap::from([
            ("node.battery", Value::Number(15.0)),
            ("node.charging", Value::Bool(false)),
            ("app.focused", Value::Text("Code.exe".to_owned())),
        ]);
        Condition::parse(text)
            .unwrap()
            .eval(&|name| facts.get(name).cloned())
    }

    #[test]
    fn comparisons_logic_and_missing_facts() {
        assert!(check("node.battery < 20"));
        assert!(check("node.battery < 20 and not node.charging"));
        assert!(!check("node.charging"));
        assert!(check("app.focused == 'code.exe'"));
        assert!(check("app.focused != \"firefox.exe\""));
        assert!(check("(node.battery >= 90 or node.battery <= 15) and true"));
        assert!(!check("screen.locked"));
        assert!(!check("screen.locked == false"));
        assert!(check("not screen.locked"));
        assert!(!check("node.battery == 'low'"));
    }

    #[test]
    fn bad_conditions_say_why() {
        for (text, reason) in [
            ("node.battery <", "ends too early"),
            ("node.battery = 20", "unknown operator"),
            ("(node.charging", "missing )"),
            ("app.focused == 'code", "not closed"),
            ("node.charging node.battery", "unexpected text"),
            ("and node.charging", "expected a value"),
        ] {
            let err = Condition::parse(text).unwrap_err();
            assert!(err.reason.contains(reason), "{text}: {err}");
        }
        assert!(Condition::parse(&"(".repeat(600)).is_err());
    }
}
