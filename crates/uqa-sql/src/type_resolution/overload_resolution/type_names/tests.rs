//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn routine_names_preserve_alias_array_quote_and_modifier_normalization() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, expected) in [
        ("PG_CATALOG.INTEGER", "int4"),
        (" TIMESTAMP ( 3 )  WITH TIME ZONE [] ", "timestamptz[]"),
        ("numeric(10, -2)[][]", "numeric[]"),
        ("integer [] []", "int4[]"),
        ("character   varying (5)", "varchar"),
        ("\"Custom (type)\"", "\"custom (type)\""),
        ("\"a\"\"(b)\"(4)", "\"a\"\"(b)\""),
        ("x(one(two)) y", "x y"),
        ("x(1)y", "xy"),
    ] {
        let result = canonical_routine_type_name_with_control(source, &control).unwrap();
        assert_eq!(&*result, expected);
        assert_eq!(canonical_routine_type_name(source), expected);
        assert_eq!(result.reserved_bytes(), result.capacity());
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn routine_array_identity_keeps_element_identity_without_declared_rank() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "array_bound_element".into(),
        oid: 42,
        array_oid: None,
        base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
        collation_oid: None,
    };
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    // PostgreSQL routine identity uses one array OID, independently of declaration rank.
    for (mut ty, identity) in [(ColumnType::Integer, "int4[]"), (domain, "domain#42[]")] {
        for rank in 1..=6 {
            ty = ColumnType::Array(Box::new(ty));
            let result = canonical_column_type_name_with_control(&ty, &control).unwrap();
            assert_eq!(result.as_str(), identity);
            assert!(ty.sql_name().ends_with(&"[]".repeat(rank)));
            assert_eq!(budget.used(), result.reserved_bytes());
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn routine_name_failure_releases_partial_scratch_and_keeps_cancellation() {
    let budget = MemoryBudget::new(16);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(canonical_routine_type_name_with_control(
        "custom_type_name_that_needs_more_than_one_buffer",
        &control
    )
    .is_err());
    assert!(budget.peak() > 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            canonical_routine_type_name_with_control("", &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn builtin_scalar_overload_names_match_sql_name_normalization() {
    let control = ProductionControl::uncontrolled();
    for ty in [
        ColumnType::SmallInteger,
        ColumnType::Integer,
        ColumnType::BigInteger,
        ColumnType::Real,
        ColumnType::DoublePrecision,
        ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        ColumnType::Numeric {
            precision: Some(15),
            scale: Some(2),
        },
        ColumnType::Numeric {
            precision: Some(10),
            scale: Some(-2),
        },
        ColumnType::Boolean,
        ColumnType::Text,
        ColumnType::Name,
        ColumnType::Varchar(None),
        ColumnType::Varchar(Some(25)),
        ColumnType::Bpchar,
        ColumnType::Character(1),
        ColumnType::Character(10),
        ColumnType::Date,
        ColumnType::Time,
        ColumnType::TimePrecision(3),
        ColumnType::TimeTz,
        ColumnType::TimeTzPrecision(0),
        ColumnType::Timestamp,
        ColumnType::TimestampPrecision(6),
        ColumnType::TimestampTz,
        ColumnType::TimestampTzPrecision(2),
        ColumnType::Interval,
        ColumnType::Bytea,
        ColumnType::Uuid,
        ColumnType::Json,
        ColumnType::JsonB,
        ColumnType::Oid,
    ] {
        let sql_name = ty.sql_name_with_control(&control).unwrap();
        let normalized = canonical_routine_type_name_with_control(&sql_name, &control).unwrap();
        assert_eq!(
            builtin_canonical_type_name(&ty),
            Some(normalized.as_str()),
            "{ty:?} spelled {}",
            sql_name.as_str()
        );
        assert_eq!(canonical_column_type_name(&ty), normalized.as_str());
    }
    for ty in [
        ColumnType::Array(Box::new(ColumnType::Integer)),
        ColumnType::Named("custom".into()),
        ColumnType::Bit(Some(3)),
        ColumnType::IntervalWithFields {
            fields: crate::ast::IntervalFields::YearToMonth,
            precision: None,
        },
    ] {
        assert_eq!(builtin_canonical_type_name(&ty), None, "{ty:?}");
    }
}

#[test]
fn builtin_scalar_overload_names_retain_exact_reservations() {
    let budget = MemoryBudget::new(64);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let name = canonical_column_type_name_with_control(
        &ColumnType::Numeric {
            precision: Some(15),
            scale: Some(2),
        },
        &control,
    )
    .unwrap();
    assert_eq!(&*name, "numeric");
    assert_eq!(name.reserved_bytes(), name.capacity());
    assert_eq!(budget.used(), name.reserved_bytes());
    drop(name);
    assert_eq!(budget.used(), 0);
}
