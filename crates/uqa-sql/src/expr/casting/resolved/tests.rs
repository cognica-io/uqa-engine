//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn resolved_scalar_kernels_preserve_postgresql_values_and_named_casts() {
    let control = ProductionControl::uncontrolled();
    let cases = [
        (
            "real",
            "varchar(12)",
            Value::Float(1.234_567_89),
            Value::Str("1.2345679".into()),
        ),
        (
            "double precision",
            "text",
            Value::Float(1.234_567_89),
            Value::Str("1.23456789".into()),
        ),
        (
            "text",
            "varchar(3)",
            Value::Str("한글abcd".into()),
            Value::Str("한글a".into()),
        ),
        (
            "text",
            "character(3)",
            Value::Str("x".into()),
            Value::FixedChar("x  ".into()),
        ),
        (
            "bpchar",
            "bpchar",
            Value::FixedChar("x  ".into()),
            Value::FixedChar("x  ".into()),
        ),
        (
            "bpchar",
            "text",
            Value::FixedChar("x  ".into()),
            Value::Str("x".into()),
        ),
        (
            "text",
            "boolean",
            Value::Str("yes".into()),
            Value::Bool(true),
        ),
        (
            "text",
            "uuid",
            Value::Str("00112233445566778899AABBCCDDEEFF".into()),
            Value::Str("00112233-4455-6677-8899-aabbccddeeff".into()),
        ),
        ("boolean", "integer", Value::Bool(true), Value::Int(1)),
        (
            "\"char\"",
            "integer",
            Value::Str("A".into()),
            Value::Int(65),
        ),
        (
            "\"char\"",
            "text",
            Value::Str("A".into()),
            Value::Str("A".into()),
        ),
    ];
    for (source, target, input, expected) in cases {
        let source_type = ColumnType::from_sql_name(source).unwrap();
        let target_type = ColumnType::from_sql_name(target).unwrap();
        let cast = ResolvedCast::select(&source_type, &target_type).unwrap();
        assert_eq!(
            *cast.evaluate(&input, &control).unwrap(),
            expected,
            "{source} -> {target}"
        );
        assert_eq!(
            super::super::cast_value_from(&input, target, Some(source)).unwrap(),
            expected
        );
        assert_eq!(*cast.evaluate(&Value::Null, &control).unwrap(), Value::Null);
    }
    for target in ["json", "jsonb", "numeric(3,1)"] {
        let input = Value::Str(
            if target.starts_with("numeric") {
                "1.45"
            } else {
                r#"{"a": 1, "b": [true, null]}"#
            }
            .into(),
        );
        let target_type = ColumnType::from_sql_name(target).unwrap();
        let cast = ResolvedCast::select(&ColumnType::Text, &target_type).unwrap();
        assert_eq!(
            *cast.evaluate(&input, &control).unwrap(),
            super::super::cast_value_from(&input, target, Some("text")).unwrap()
        );
    }
}

#[test]
fn resolved_scalar_kernels_keep_resource_errors_and_exclude_live_type_identities() {
    assert!(std::mem::size_of::<ResolvedCast>() <= 64);
    let cast = ResolvedCast::select(&ColumnType::Real, &ColumnType::Text).unwrap();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        cast.evaluate(&Value::Float(1.5), &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    assert_eq!(*cast.evaluate(&Value::Null, &control).unwrap(), Value::Null);
    token.cancel();
    assert_eq!(
        cast.evaluate(&Value::Null, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    for ty in [
        ColumnType::Oid,
        ColumnType::Regtype,
        ColumnType::Record,
        ColumnType::Array(Box::new(ColumnType::Integer)),
        ColumnType::Named("public.dynamic_type".into()),
    ] {
        assert!(ResolvedCast::select(&ty, &ColumnType::Text).is_none());
        assert!(ResolvedCast::select(&ColumnType::Text, &ty).is_none());
    }
}

#[test]
fn resolved_temporal_modifiers_preserve_postgresql_rounding_and_field_rules() {
    use uqa_core::TemporalValue;
    let control = ProductionControl::uncontrolled();
    let cases = [
        (
            ColumnType::TimestampPrecision(2),
            "2000-01-01 12:34:56.789",
            TemporalValue::Timestamp {
                postgres_micros: (946_730_096_790_000) - uqa_core::POSTGRES_EPOCH_MICROS,
            },
        ),
        (
            ColumnType::TimePrecision(2),
            "12:34:56.789",
            TemporalValue::Time {
                micros: 45_296_790_000,
            },
        ),
        (
            ColumnType::IntervalWithFields {
                fields: crate::ast::IntervalFields::DayToSecond,
                precision: Some(2),
            },
            "2 years 3 mons 4 days 12:34:56.789",
            TemporalValue::Interval {
                months: 27,
                days: 4,
                micros: 45_296_790_000,
            },
        ),
        (
            ColumnType::IntervalWithFields {
                fields: crate::ast::IntervalFields::Year,
                precision: None,
            },
            "2 years 3 mons 4 days 12:34:56.789",
            TemporalValue::Interval {
                months: 24,
                days: 0,
                micros: 0,
            },
        ),
    ];
    for (target, text, expected) in cases {
        let cast = ResolvedCast::select(&ColumnType::Text, &target).unwrap();
        assert_eq!(
            *cast.evaluate(&Value::Str(text.into()), &control).unwrap(),
            Value::Temporal(expected),
        );
        assert_eq!(*cast.evaluate(&Value::Null, &control).unwrap(), Value::Null);
    }
}
