//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Filter predicates evaluated against [`Value`] field contents.
//!
//! `Predicate` is an enum (rather than a trait) so callers can pattern
//! match on the special `IsNull` / `IsNotNull` cases that need to see the
//! `None` field value, while everything else short-circuits on missing
//! fields. `Like` / `ILike` regex variants land alongside the SQL
//! compiler.

use std::collections::BTreeSet;

use crate::types::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Equals(Value),
    NotEquals(Value),
    GreaterThan(Value),
    GreaterThanOrEqual(Value),
    LessThan(Value),
    LessThanOrEqual(Value),
    InSet(BTreeSet<Value>),
    Between { low: Value, high: Value },
    IsNull,
    IsNotNull,
}

impl Predicate {
    /// Returns `true` if the predicate must inspect `None` field values
    /// (the `IsNull` / `IsNotNull` case). Filter operators short-circuit
    /// missing fields for every other variant.
    pub fn is_null_aware(&self) -> bool {
        matches!(self, Predicate::IsNull | Predicate::IsNotNull)
    }

    /// Evaluate against an optional field value. The two null-aware
    /// variants see the `None` directly; the rest reject `None`.
    pub fn evaluate(&self, value: Option<&Value>) -> bool {
        match self {
            Predicate::IsNull => value.is_none(),
            Predicate::IsNotNull => value.is_some(),
            Predicate::Equals(target) => value == Some(target),
            Predicate::NotEquals(target) => value.is_some() && value != Some(target),
            Predicate::GreaterThan(target) => value.is_some_and(|v| v > target),
            Predicate::GreaterThanOrEqual(target) => value.is_some_and(|v| v >= target),
            Predicate::LessThan(target) => value.is_some_and(|v| v < target),
            Predicate::LessThanOrEqual(target) => value.is_some_and(|v| v <= target),
            Predicate::InSet(values) => value.is_some_and(|v| values.contains(v)),
            Predicate::Between { low, high } => value.is_some_and(|v| v >= low && v <= high),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(n: i64) -> Value {
        Value::Int(n)
    }

    #[test]
    fn equals_matches_exact() {
        let p = Predicate::Equals(iv(42));
        assert!(p.evaluate(Some(&iv(42))));
        assert!(!p.evaluate(Some(&iv(43))));
        assert!(!p.evaluate(None));
    }

    #[test]
    fn comparators_respect_ordering() {
        let p = Predicate::GreaterThan(iv(10));
        assert!(p.evaluate(Some(&iv(11))));
        assert!(!p.evaluate(Some(&iv(10))));
        assert!(!p.evaluate(Some(&iv(9))));
    }

    #[test]
    fn between_inclusive_bounds() {
        let p = Predicate::Between {
            low: iv(1),
            high: iv(3),
        };
        assert!(p.evaluate(Some(&iv(1))));
        assert!(p.evaluate(Some(&iv(3))));
        assert!(!p.evaluate(Some(&iv(0))));
        assert!(!p.evaluate(Some(&iv(4))));
    }

    #[test]
    fn in_set_membership() {
        let s: BTreeSet<Value> = [iv(1), iv(2), iv(5)].into_iter().collect();
        let p = Predicate::InSet(s);
        assert!(p.evaluate(Some(&iv(2))));
        assert!(!p.evaluate(Some(&iv(3))));
    }

    #[test]
    fn null_aware_predicates_see_none() {
        assert!(Predicate::IsNull.evaluate(None));
        assert!(!Predicate::IsNull.evaluate(Some(&iv(0))));
        assert!(!Predicate::IsNotNull.evaluate(None));
        assert!(Predicate::IsNotNull.evaluate(Some(&iv(0))));
    }
}
