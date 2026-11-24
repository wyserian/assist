//! Facts: current device state such as `node.battery` or `app.focused` (PLAN.md section
//! 3.10). Adapters set them; conditions read them. Facts are not stored or synced.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

/// Most facts kept; a source that keeps inventing names cannot grow memory.
const MAX_FACTS: usize = 256;
/// Longest text value kept, in bytes.
const MAX_TEXT: usize = 256;

/// A fact's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Yes or no, e.g. `node.charging`.
    Bool(bool),
    /// A number, e.g. `node.battery` in percent.
    Number(f64),
    /// Text, e.g. `app.focused`.
    Text(String),
}

impl Value {
    /// Whether the value counts as true in a condition: true, a number other than 0, or
    /// non-empty text.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::Text(t) => !t.is_empty(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::Text(t) => write!(f, "{t:?}"),
        }
    }
}

/// The current facts. Cheap to clone; clones share the same store.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    values: Arc<RwLock<BTreeMap<String, Value>>>,
}

impl Facts {
    /// Sets a fact. Text is cut to a bounded length; a new name past the limit is dropped.
    pub fn set(&self, name: &str, value: Value) {
        let value = match value {
            Value::Text(mut text) if text.len() > MAX_TEXT => {
                let mut end = MAX_TEXT;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                Value::Text(text)
            }
            other => other,
        };
        let mut values = self.values.write().unwrap_or_else(PoisonError::into_inner);
        if values.len() < MAX_FACTS || values.contains_key(name) {
            values.insert(name.to_owned(), value);
        }
    }

    /// Removes a fact, e.g. when its source can no longer tell.
    pub fn remove(&self, name: &str) {
        self.values
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }

    /// One fact.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.values
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Every fact, by name.
    pub fn all(&self) -> BTreeMap<String, Value> {
        self.values
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove_and_bounds() {
        let facts = Facts::default();
        facts.set("node.battery", Value::Number(80.0));
        assert_eq!(facts.get("node.battery"), Some(Value::Number(80.0)));
        facts.set("app.focused", Value::Text("x".repeat(1000)));
        assert!(matches!(facts.get("app.focused"), Some(Value::Text(t)) if t.len() == MAX_TEXT));
        facts.remove("node.battery");
        assert_eq!(facts.get("node.battery"), None);
        for i in 0..MAX_FACTS + 10 {
            facts.set(&format!("f{i}"), Value::Bool(true));
        }
        assert_eq!(facts.all().len(), MAX_FACTS);
    }
}
