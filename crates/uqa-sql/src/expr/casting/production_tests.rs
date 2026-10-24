//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn multidimensional_casts_convert_scalar_leaves_and_retain_bounds_under_control() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let text = Value::Str("[-2:-1][4:5]={{1,2},{3,4}}".into());
    let expected = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![
                Value::List(vec![Value::Int(1), Value::Int(2)]),
                Value::List(vec![Value::Int(3), Value::Int(4)]),
            ],
            vec![-2, 4],
        )
        .unwrap(),
    );
    for target in ["integer[][]", "integer [] []", "integer[][][]"] {
        let output = cast_value_from_with_control(&text, target, None, &control).unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let output =
        cast_value_from_with_control(&expected, "bigint[][]", Some("integer[][]"), &control)
            .unwrap();
    assert_eq!(*output, expected);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    let error = cast_value_from_with_control(
        &Value::Str("{{1,invalid}}".into()),
        "integer[][]",
        None,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));
    assert_eq!(budget.used(), 0);
    token.cancel();
    let error = cast_value_from_with_control(&text, "integer[][]", None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 0);
}

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
            Value::FixedChar("é  ".into()),
            "bpchar",
            Some("bpchar"),
            Value::FixedChar("é  ".into()),
        ),
        (
            Value::FixedChar("é  ".into()),
            "text",
            Some("bpchar"),
            Value::Str("é".into()),
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

#[test]
fn dense_casts_share_assignment_dimensions_finite_values_and_admission() {
    let budget = MemoryBudget::new(65536);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let row = vec![Value::Float(-0.0), Value::Float(f64::from(f32::MAX))];
    let vector = Value::Array(ArrayValue::try_new(row.clone()).unwrap());
    let tensor = Value::Array(ArrayValue::try_new(vec![Value::List(row)]).unwrap());
    for (value, target, ty) in [
        (&vector, "vector(2)", crate::ColumnType::Vector(2)),
        (&tensor, "tensor(2)", crate::ColumnType::Tensor(2)),
        (
            &Value::List(vec![]),
            "tensor(2)",
            crate::ColumnType::Tensor(2),
        ),
    ] {
        let result = cast_value_from_with_control(value, target, None, &control).unwrap();
        let assigned = crate::assignment::conversion::convert_value_to_column_type_with_control(
            control.copy_value(value).unwrap(),
            &ty,
            &control,
        )
        .unwrap();
        assert_eq!(*result, *assigned);
        assert_eq!(
            budget.used(),
            result.reserved_bytes() + assigned.reserved_bytes()
        );
    }
    assert_eq!(budget.used(), 0);
    let converted =
        cast_value_from_with_control(&vector, "pg_catalog.\"vector\"(2)", None, &control).unwrap();
    let Value::List(values) = &*converted else {
        panic!("dense list");
    };
    let Value::Float(zero) = values[0] else {
        panic!("float32 element");
    };
    assert_eq!(zero.to_bits(), (-0.0_f64).to_bits());
    drop(converted);
    for (value, target) in [
        (&vector, "vector(1)"),
        (&tensor, "tensor(1)"),
        (&vector, "tensor(2)"),
        (&tensor, "vector(2)"),
        (&Value::List(vec![Value::Null]), "vector(1)"),
        (&Value::List(vec![Value::Float(f64::INFINITY)]), "vector(1)"),
    ] {
        assert!(cast_value_from_with_control(value, target, None, &control).is_err());
        assert_eq!(budget.used(), 0);
    }
    let held = control.copy_text(&"x".repeat(65536)).unwrap();
    assert_eq!(
        cast_value_from_with_control(&vector, "vector(2)", None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), held.reserved_bytes());
    drop(held);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            cast_value_from_with_control(&Value::List(vec![]), "tensor(2)", None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    assert_eq!(budget.used(), 0);
}
