//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{IntervalFields, RangeSubtype};
use uqa_core::{
    memory::{MemoryBudget, MemoryError},
    CancellationToken,
};

fn names() -> Vec<(ColumnType, &'static str, &'static str)> {
    vec![
        (ColumnType::SmallInteger, "smallint", "smallint"),
        (ColumnType::InternalChar, "\"char\"", "\"char\""),
        (
            ColumnType::Varchar(Some(19)),
            "character varying(19)",
            "character varying",
        ),
        (ColumnType::Character(7), "character(7)", "character"),
        (ColumnType::Bpchar, "bpchar", "character"),
        (
            ColumnType::Numeric {
                precision: Some(12),
                scale: Some(-3),
            },
            "numeric(12,-3)",
            "numeric",
        ),
        (
            ColumnType::Numeric {
                precision: Some(12),
                scale: None,
            },
            "numeric",
            "numeric",
        ),
        (
            ColumnType::TimePrecision(2),
            "time(2) without time zone",
            "time without time zone",
        ),
        (
            ColumnType::TimestampTzPrecision(6),
            "timestamp(6) with time zone",
            "timestamp with time zone",
        ),
        (
            ColumnType::IntervalWithFields {
                fields: IntervalFields::DayToSecond,
                precision: Some(4),
            },
            "interval day to second(4)",
            "interval",
        ),
        (
            ColumnType::IntervalWithFields {
                fields: IntervalFields::Year,
                precision: None,
            },
            "interval year",
            "interval",
        ),
        (
            ColumnType::Range(RangeSubtype::Integer),
            "int4range",
            "int4range",
        ),
        (
            ColumnType::Multirange(RangeSubtype::TimestampTz),
            "tstzmultirange",
            "tstzmultirange",
        ),
        (
            ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Varchar(
                Some(2),
            ))))),
            "character varying(2)[][]",
            "character varying[][]",
        ),
        (
            ColumnType::Domain {
                schema: "a\"B".into(),
                name: "한.글".into(),
                oid: 42,
                base: Box::new(ColumnType::Text),
            },
            "\"a\"\"B\".\"한.글\"",
            "\"a\"\"B\".\"한.글\"",
        ),
        (
            ColumnType::Domain {
                schema: "bare".into(),
                name: "a1_$".into(),
                oid: 43,
                base: Box::new(ColumnType::Integer),
            },
            "bare.a1_$",
            "bare.a1_$",
        ),
        (ColumnType::Vector(5), "vector(5)", "vector"),
        (ColumnType::Tensor(9), "tensor(9)", "tensor"),
        (
            ColumnType::Named("custom spelling".into()),
            "custom spelling",
            "custom spelling",
        ),
    ]
}

#[test]
fn ordinary_and_admitted_names_preserve_declared_modifiers_and_regtype_identity() {
    let budget = MemoryBudget::new(1 << 16);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (ty, sql, regtype) in names() {
        assert_eq!(ty.sql_name(), sql);
        assert_eq!(ty.regtype_name(), regtype);
        let written = ty.sql_name_with_control(&control).unwrap();
        assert_eq!(&*written, sql);
        assert_eq!(written.reserved_bytes(), written.capacity());
        let reg = ty.regtype_name_with_control(&control).unwrap();
        assert_eq!(&*reg, regtype);
        assert_eq!(budget.used(), written.capacity() + reg.capacity());
        drop(written);
        drop(reg);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn rejected_type_names_cleanup_and_both_cancellation_scopes_remain_active() {
    let ty = ColumnType::Domain {
        schema: "quoted.schema".repeat(200),
        name: "name".repeat(200),
        oid: 1,
        base: Box::new(ColumnType::Text),
    };
    for cancelled in [None, Some(true), Some(false)] {
        let budget = MemoryBudget::new(128);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        match cancelled {
            Some(true) => original.cancel(),
            Some(false) => invoking.cancel(),
            None => {}
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = ty.sql_name_with_control(&control);
        match cancelled {
            None => assert!(matches!(
                result,
                Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
            )),
            Some(_) => assert!(matches!(result, Err(ValueRetentionError::Cancelled(_)))),
        }
        assert_eq!(budget.used(), 0);
    }
}
