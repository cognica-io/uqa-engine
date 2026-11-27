//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiled string patterns owned by one read or write statement.

use std::{cell::RefCell, collections::VecDeque};

use super::CypherError;
use regex::Regex;
use uqa_core::Value;

const MAX_PATTERNS: usize = 8;
const MAX_RETAINED_SOURCE: usize = 4096;

#[derive(Default)]
pub(in crate::cypher) struct PatternCache(RefCell<VecDeque<Regex>>);

impl PatternCache {
    /// AGE uses an unanchored search and returns null for non-string operands.
    pub(super) fn matches(&self, lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
        let (Value::Str(text), Value::Str(pattern)) = (lhs, rhs) else {
            return Ok(Value::Null);
        };
        let mut cached = self.0.borrow_mut();
        if let Some(compiled) = cached.iter().find(|entry| entry.as_str() == pattern) {
            return Ok(Value::Bool(compiled.is_match(text)));
        }
        let compiled = Regex::new(pattern)
            .map_err(|e| CypherError::TypeError(format!("invalid regular expression: {e}")))?;
        let result = compiled.is_match(text);
        if pattern.len() <= MAX_RETAINED_SOURCE {
            if cached.len() == MAX_PATTERNS {
                cached.pop_back();
            }
            cached.push_front(compiled);
        }
        Ok(Value::Bool(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_cypher_patterns_match_without_allocating_or_recompiling() {
        let patterns = PatternCache::default();
        let word = Value::Str("coffee bean".into());
        let pattern = Value::Str("be(a|e)n".into());
        assert_eq!(
            patterns.matches(&word, &pattern).unwrap(),
            Value::Bool(true)
        );
        let allocated = allocation_counter::measure(|| {
            for _ in 0..2048 {
                assert_eq!(
                    patterns.matches(&word, &pattern).unwrap(),
                    Value::Bool(true)
                );
            }
        });
        assert_eq!(allocated.count_total, 0);
        assert_eq!(patterns.0.borrow().len(), 1);
    }

    #[test]
    fn cache_retention_preserves_null_invalid_and_oversized_pattern_results() {
        let patterns = PatternCache::default();
        let text = Value::Str("abc".into());
        let invalid = Value::Str("[".into());
        assert_eq!(
            patterns.matches(&Value::Null, &invalid).unwrap(),
            Value::Null
        );
        assert_eq!(
            patterns.matches(&text, &Value::Int(3)).unwrap(),
            Value::Null
        );
        assert!(
            matches!(patterns.matches(&text, &invalid), Err(CypherError::TypeError(message)) if message.starts_with("invalid regular expression:"))
        );
        let long = Value::Str(format!("(?x)b#{}", "x".repeat(MAX_RETAINED_SOURCE)));
        assert_eq!(patterns.matches(&text, &long).unwrap(), Value::Bool(true));
        assert!(patterns.0.borrow().is_empty());
        for index in 0..(MAX_PATTERNS + 2) {
            patterns
                .matches(&text, &Value::Str(index.to_string()))
                .unwrap();
        }
        let entries = patterns.0.borrow();
        assert_eq!(entries.len(), MAX_PATTERNS);
        assert!(entries
            .iter()
            .all(|p| p.as_str() != "0" && p.as_str() != "1"));
    }
}
