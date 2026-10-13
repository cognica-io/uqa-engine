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
        collation_oid: None,
        schema: "custom".into(),
        name: "numeric_values".into(),
        oid: 40000,
        array_oid: None,
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
        "\"pg_catalog\".\"varchar\"(15)[]",
        "PG_CATALOG.INTERVAL(32767,3)",
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
        "pg_catalog.\"TEXT\"",
        "pg_catalog.integer",
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
        collation_oid: None,
        schema: "a".repeat(64),
        name: "b".repeat(64),
        oid: 40000,
        array_oid: None,
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

#[test]
fn catalog_names_retain_bpchar_input_length_and_type_modifiers() {
    let budget = MemoryBudget::new(1 << 16);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (spelling, expected) in [
        ("bpchar", ColumnType::Bpchar),
        (r#""bpchar""#, ColumnType::Bpchar),
        ("pg_catalog.bpchar", ColumnType::Bpchar),
        (r#"pg_catalog."bpchar""#, ColumnType::Bpchar),
        (r#""pg_catalog"."bpchar""#, ColumnType::Bpchar),
        (
            "pg_catalog.bpchar[]",
            ColumnType::Array(Box::new(ColumnType::Bpchar)),
        ),
        ("pg_catalog.bpchar(7)", ColumnType::Character(7)),
        (
            r#"pg_catalog."bpchar"(7)[]"#,
            ColumnType::Array(Box::new(ColumnType::Character(7))),
        ),
        ("character", ColumnType::Character(1)),
        ("character(7)", ColumnType::Character(7)),
        (r#"pg_catalog."char""#, ColumnType::InternalChar),
    ] {
        assert_eq!(
            ColumnType::from_sql_name(spelling).unwrap(),
            expected,
            "{spelling}"
        );
        let actual = ColumnType::from_sql_name_with_control(spelling, &control).unwrap();
        assert_eq!(*actual, expected, "{spelling}");
        assert_eq!(budget.used(), actual.reserved_bytes());
        drop(actual);
        assert_eq!(budget.used(), 0);
    }

    for (name, diagnostic) in [
        ("vector", "VECTOR requires a dimension"),
        ("tensor", "TENSOR requires a dimension"),
    ] {
        for spelling in [
            format!("pg_catalog.{name}"),
            format!(r#""{name}""#),
            format!(r#"pg_catalog."{name}""#),
            format!(r#""pg_catalog"."{name}""#),
            format!("pg_catalog.{name}[]"),
        ] {
            for error in [
                ColumnType::from_sql_name(&spelling).unwrap_err(),
                ColumnType::from_sql_name_with_control(&spelling, &control).unwrap_err(),
            ] {
                assert!(
                    matches!(error, crate::SQLError::TypeMismatch(ref message) if message == diagnostic),
                    "{spelling}: {error}",
                );
            }
            assert_eq!(budget.used(), 0);
        }
    }
}
