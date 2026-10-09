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
    /// variants treat both an absent field (`None`) and an explicit
    /// `Value::Null` as null, and test a row value field by field as
    /// [`sql_null_test`] does; the rest reject a null value.
    pub fn evaluate(&self, value: Option<&Value>) -> bool {
        let is_null = matches!(value, None | Some(Value::Null));
        match self {
            Predicate::IsNull => sql_null_test(value, false),
            Predicate::IsNotNull => sql_null_test(value, true),
            Predicate::Equals(target) => !is_null && value.is_some_and(|v| values_equal(v, target)),
            Predicate::NotEquals(target) => {
                !is_null && value.is_some_and(|v| !values_equal(v, target))
            }
            Predicate::GreaterThan(target) => {
                !is_null && value.is_some_and(|v| compare_values(v, target).is_gt())
            }
            Predicate::GreaterThanOrEqual(target) => {
                !is_null && value.is_some_and(|v| compare_values(v, target).is_ge())
            }
            Predicate::LessThan(target) => {
                !is_null && value.is_some_and(|v| compare_values(v, target).is_lt())
            }
            Predicate::LessThanOrEqual(target) => {
                !is_null && value.is_some_and(|v| compare_values(v, target).is_le())
            }
            Predicate::InSet(values) => {
                !is_null
                    && value.is_some_and(|v| values.iter().any(|target| values_equal(v, target)))
            }
            Predicate::Between { low, high } => {
                !is_null
                    && value.is_some_and(|v| {
                        compare_values(v, low).is_ge() && compare_values(v, high).is_le()
                    })
            }
        }
    }
}

/// SQL `IS NULL` (or `IS NOT NULL` when `negated`), as `PostgreSQL`'s `NullTest` evaluates it: a row value is null when every field is null and not null when no field is, so a row with both kinds of fields is neither; any other value is null only when it is the null value. An absent field is null.
#[must_use]
pub fn sql_null_test(value: Option<&Value>, negated: bool) -> bool {
    let fields: Box<dyn Iterator<Item = &Value>> = match value {
        None | Some(Value::Null) => return !negated,
        Some(Value::Row(values)) => Box::new(values.iter()),
        Some(Value::Record(fields)) => Box::new(fields.iter().map(|(_, value)| value)),
        Some(_) => return negated,
    };
    let mut fields = fields;
    if negated {
        fields.all(|field| !matches!(field, Value::Null))
    } else {
        fields.all(|field| matches!(field, Value::Null))
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Temporal(x), Value::Temporal(y)) => x == y,
        (Value::Temporal(x), Value::Str(y)) | (Value::Str(y), Value::Temporal(x)) => {
            x.parse_same_kind(y).is_some_and(|parsed| parsed == *x)
        }
        (Value::FixedChar(x) | Value::Str(x), Value::FixedChar(y))
        | (Value::FixedChar(x), Value::Str(y)) => {
            x.trim_end_matches(' ') == y.trim_end_matches(' ')
        }
        _ => a == b,
    }
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map_or_else(|| a.cmp(b), |parsed| x.cmp(&parsed)),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map_or_else(|| a.cmp(b), |parsed| parsed.cmp(y)),
        (Value::FixedChar(x) | Value::Str(x), Value::FixedChar(y))
        | (Value::FixedChar(x), Value::Str(y)) => {
            x.trim_end_matches(' ').cmp(y.trim_end_matches(' '))
        }
        _ => a.cmp(b),
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

    #[test]
    fn fixed_character_predicates_ignore_blank_padding() {
        let fixed = Value::FixedChar("x   ".into());
        assert!(Predicate::Equals(Value::Str("x".into())).evaluate(Some(&fixed)));
        assert!(Predicate::Equals(Value::Str("x  ".into())).evaluate(Some(&fixed)));
        assert!(Predicate::LessThan(Value::Str("y".into())).evaluate(Some(&fixed)));
    }
}

#[cfg(test)]
mod null_test_tests {
    use super::{sql_null_test, Predicate};
    use crate::types::Value;

    #[test]
    fn row_values_are_null_only_when_every_field_is() {
        let all_null = Value::Row(vec![Value::Null, Value::Null].into());
        let mixed =
            Value::Record(vec![("x".into(), Value::Int(1)), ("y".into(), Value::Null)].into());
        let none_null = Value::Row(vec![Value::Int(1)].into());
        let empty = Value::Row(Vec::new().into());
        for (value, is_null, is_not_null) in [
            (Some(&all_null), true, false),
            (Some(&mixed), false, false),
            (Some(&none_null), false, true),
            (Some(&empty), true, true),
            (Some(&Value::Null), true, false),
            (None, true, false),
            (Some(&Value::Int(0)), false, true),
        ] {
            assert_eq!(sql_null_test(value, false), is_null, "{value:?}");
            assert_eq!(sql_null_test(value, true), is_not_null, "{value:?}");
            assert_eq!(Predicate::IsNull.evaluate(value), is_null);
            assert_eq!(Predicate::IsNotNull.evaluate(value), is_not_null);
        }
    }
}
