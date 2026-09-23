//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_type_clones_hold_every_domain_and_array_allocation() {
    let ty = ColumnType::Domain {
        schema: "custom".into(),
        name: "numeric_values".into(),
        oid: 40000,
        base: Box::new(ColumnType::Array(Box::new(ColumnType::Numeric {
            precision: Some(10),
            scale: Some(-2),
        }))),
    };
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let copy = ty.clone_with_control(&control).unwrap();
    assert_eq!(*copy, ty);
    let ColumnType::Domain { schema, name, .. } = &*copy else {
        panic!("domain preserved")
    };
    assert_eq!(
        copy.reserved_bytes(),
        schema.capacity() + name.capacity() + 2 * size_of::<ColumnType>()
    );
    assert_eq!(budget.used(), copy.reserved_bytes());
    drop(copy);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_type_parser_preserves_aliases_modifiers_and_errors() {
    let budget = MemoryBudget::new(1 << 16);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for spelling in [
        " _INT4 ",
        "numeric(10,-2)[][]",
        "TIMESTAMP(3) WITH TIME ZONE",
        "interval day to second(4)",
        "pg_catalog.varchar(15)",
        "int8multirange",
        "vector(8)",
        "\"char\"",
        "oidvector",
    ] {
        let expected = ColumnType::from_sql_name(spelling).unwrap();
        let actual = ColumnType::from_sql_name_with_control(spelling, &control).unwrap();
        assert_eq!(*actual, expected);
        assert_eq!(budget.used(), actual.reserved_bytes());
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
    for spelling in [
        "void[]",
        "character(0)",
        "numeric(2,3,4)",
        "time(-1)",
        "unknown_type",
    ] {
        let expected = ColumnType::from_sql_name(spelling).unwrap_err();
        let actual = ColumnType::from_sql_name_with_control(spelling, &control).unwrap_err();
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(actual.sqlstate(), expected.sqlstate());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scalar_type_payloads_and_removed_modifiers_require_no_heap_lease() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for ty in [
        ColumnType::Integer,
        ColumnType::Varchar(Some(5)),
        ColumnType::TimestampPrecision(2),
    ] {
        assert_eq!(*ty.clone_with_control(&control).unwrap(), ty);
        assert_eq!(
            *ty.without_type_modifiers_with_control(&control).unwrap(),
            ty.without_type_modifiers()
        );
    }
    let parsed = ColumnType::from_sql_name_with_control("integer", &control).unwrap();
    assert_eq!(*parsed, ColumnType::Integer);
    assert_eq!(parsed.reserved_bytes(), 0);
    assert!(ColumnType::Array(Box::new(ColumnType::Integer))
        .clone_with_control(&control)
        .is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn type_production_checks_both_tokens_and_releases_partial_results() {
    let ty = ColumnType::Domain {
        schema: "a".repeat(64),
        name: "b".repeat(64),
        oid: 40000,
        base: Box::new(ColumnType::Text),
    };
    let budget = MemoryBudget::new(80);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(ty.clone_with_control(&control).is_err());
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
            ColumnType::Text.clone_with_control(&control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(
            ColumnType::from_sql_name_with_control("int", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
