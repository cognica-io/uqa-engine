//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_casts_preserve_arrays_character_numeric_temporal_and_binary_semantics() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (source, target, declared, expected) in [
        (
            Value::Str("ébc".into()),
            "varchar(2)",
            None,
            Value::Str("éb".into()),
        ),
        (
            Value::Str("é".into()),
            "character(3)",
            None,
            Value::FixedChar("é  ".into()),
        ),
        (
            Value::Str("-12.345".into()),
            "numeric(4,2)",
            None,
            Value::Decimal(uqa_core::DecimalValue::parse("-12.35").unwrap()),
        ),
        (
            Value::Int(-1),
            "bytea",
            Some("smallint"),
            Value::Bytes(vec![255, 255]),
        ),
        (
            Value::Str("\\x00ff".into()),
            "bytea",
            None,
            Value::Bytes(vec![0, 255]),
        ),
        (
            Value::Str("24:00:00".into()),
            "time",
            None,
            Value::Temporal(TemporalValue::Time {
                micros: 86_400_000_000,
            }),
        ),
        (
            Value::Str("A0EEBC999C0B4EF8BB6D6BB9BD380A11".into()),
            "uuid",
            None,
            Value::Str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()),
        ),
    ] {
        let output = cast_value_from_with_control(&source, target, declared, &control).unwrap();
        assert_eq!(&*output, &expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let output = cast_value_from_with_control(
        &Value::Str("[-2:-1][4:5]={{1,2},{3,4}}".into()),
        "integer[]",
        None,
        &control,
    )
    .unwrap();
    let Value::Array(array) = &*output else {
        unreachable!();
    };
    assert_eq!(array.lower_bounds(), &[-2, 4]);
    assert_eq!(array.dimensions(), &[2, 2]);
    assert_eq!(
        array.elements(),
        &[
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)])
        ]
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn casts_reject_quota_before_large_destinations_and_release_partial_arrays() {
    let budget = MemoryBudget::new(128);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let error =
        cast_value_from_with_control(&Value::Str("x".into()), "character(4096)", None, &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    let error = cast_value_from_with_control(
        &Value::Str("{one,two,three,four}".into()),
        "text[]",
        None,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    invoking.cancel();
    let error =
        cast_value_from_with_control(&Value::Int(2), "integer", None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 0);
}
