//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn resolved_integer_conversion_needs_no_type_name_workspace() {
    let cast = NumericCast::select(&ColumnType::Integer, &ColumnType::BigInteger).unwrap();
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for value in -5000..5000 {
        assert_eq!(
            *cast.evaluate(&Value::Int(value), &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(*cast.evaluate(&Value::Null, &control).unwrap(), Value::Null);
    assert_eq!(budget.peak(), 0);
    invoking.cancel();
    assert_eq!(
        cast.evaluate(&Value::Int(1), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    let invoking = CancellationToken::new();
    original.cancel();
    assert_eq!(
        cast.evaluate(
            &Value::Int(1),
            &ProductionControl::new(&budget, &original, &invoking)
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
}

#[test]
fn resolved_numeric_casts_keep_postgresql_rounding_and_typmods() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let decimal = |text| Value::Decimal(DecimalValue::parse(text).unwrap());
    let unmodified = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    for (source, value, target, expected) in [
        (
            ColumnType::DoublePrecision,
            Value::Float(2.5),
            ColumnType::Integer,
            Value::Int(2),
        ),
        (
            unmodified.clone(),
            decimal("2.5"),
            ColumnType::Integer,
            Value::Int(3),
        ),
        (
            unmodified.clone(),
            decimal("-2.5"),
            ColumnType::Integer,
            Value::Int(-3),
        ),
        (
            ColumnType::Integer,
            Value::Int(16_777_217),
            ColumnType::Real,
            Value::Float(16_777_216.0),
        ),
        (
            ColumnType::Integer,
            Value::Int(16_777_217),
            ColumnType::DoublePrecision,
            Value::Float(16_777_217.0),
        ),
        (
            unmodified.clone(),
            decimal("-12.345"),
            ColumnType::Numeric {
                precision: Some(5),
                scale: Some(2),
            },
            decimal("-12.35"),
        ),
        (
            unmodified.clone(),
            decimal("1250"),
            ColumnType::Numeric {
                precision: Some(2),
                scale: Some(-2),
            },
            decimal("1300"),
        ),
        (
            ColumnType::Integer,
            Value::Int(42),
            unmodified.clone(),
            decimal("42"),
        ),
    ] {
        let cast = NumericCast::select(&source, &target).unwrap();
        let output = cast.evaluate(&value, &control).unwrap();
        assert_eq!(*output, expected, "{source:?} -> {target:?}");
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn resolved_numeric_casts_keep_range_errors_and_allocation_refusals() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let decimal = |text| Value::Decimal(DecimalValue::parse(text).unwrap());
    let unmodified = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    for (source, value, target) in [
        (
            ColumnType::Integer,
            Value::Int(32768),
            ColumnType::SmallInteger,
        ),
        (
            ColumnType::BigInteger,
            Value::Int(i64::from(i32::MAX) + 1),
            ColumnType::Integer,
        ),
        (
            ColumnType::DoublePrecision,
            Value::Float(f64::INFINITY),
            ColumnType::BigInteger,
        ),
        (
            unmodified,
            decimal("99.95"),
            ColumnType::Numeric {
                precision: Some(3),
                scale: Some(1),
            },
        ),
    ] {
        let error = NumericCast::select(&source, &target)
            .unwrap()
            .evaluate(&value, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
        assert_eq!(budget.used(), 0);
    }
    let refused = MemoryBudget::new(0);
    let cast = NumericCast::select(
        &ColumnType::Integer,
        &ColumnType::Numeric {
            precision: None,
            scale: None,
        },
    )
    .unwrap();
    assert_eq!(
        cast.evaluate(
            &Value::Int(1),
            &ProductionControl::new(&refused, &token, &token)
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(refused.used(), 0);
}

#[test]
fn numeric_dispatch_never_erases_non_numeric_type_identity() {
    for other in [
        ColumnType::Oid,
        ColumnType::Xid,
        ColumnType::Regtype,
        ColumnType::Boolean,
        ColumnType::Text,
        ColumnType::Array(Box::new(ColumnType::Integer)),
        ColumnType::Domain {
            schema: "public".into(),
            name: "positive".into(),
            oid: 16384,
            array_oid: None,
            collation_oid: None,
            base: Box::new(ColumnType::Integer),
        },
    ] {
        assert!(NumericCast::select(&other, &ColumnType::Integer).is_none());
        assert!(NumericCast::select(&ColumnType::Integer, &other).is_none());
    }
}
