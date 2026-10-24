//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn integer_bitwise_values_types_and_diagnostics_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_bitwise.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["expression"].as_str().unwrap();
        let expression = expression(sql);
        let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
        let actual = crate::scalar_type(&scalar, &RowSchema::default(), &[]).and_then(|ty| {
            eval(&expression, &EvalContext::new(None, &[])).map(|value| (ty, value))
        });
        if let Some(sqlstate) = case["sqlstate"].as_str() {
            let error = actual.unwrap_err();
            assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
            assert_eq!(
                error.to_string(),
                case["message"].as_str().unwrap(),
                "{sql}"
            );
        } else {
            let (ty, value) = actual.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(&ty.expect("selected result")),
                case["type_oid"].as_i64().unwrap(),
                "{sql}"
            );
            let text = (!matches!(value, Value::Null))
                .then(|| crate::expr::value_to_string(&value).unwrap());
            assert_eq!(text.as_deref(), case["value"].as_str(), "{sql}");
        }
    }
}

#[test]
fn integer_bitwise_catalog_identities_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_bitwise.json")).unwrap();
    let entries = reference["operator_catalog"].as_array().unwrap();
    assert_eq!(entries.len(), 18);
    for row in entries {
        let text = |position: usize| row[position].as_str().unwrap();
        let oid = text(4).parse().unwrap();
        let function_oid: i64 = text(5).parse().unwrap();
        assert_eq!(text(7), "t");
        if text(1).is_empty() {
            let entry = crate::unary_operator_by_oid(oid)
                .unwrap_or_else(|| panic!("missing unary operator {oid}"));
            assert_eq!(entry.name, text(0));
            assert_eq!(entry.function_oid, function_oid);
            assert_eq!(
                entry.operand_type,
                ColumnType::from_sql_name(text(2)).unwrap()
            );
        } else {
            let entry = crate::binary_operator_by_oid(oid)
                .unwrap_or_else(|| panic!("missing binary operator {oid}"));
            assert_eq!(entry.name, text(0));
            assert_eq!(entry.function_oid, function_oid);
            assert_eq!(entry.operand_types, [text(1), text(2)]);
            assert_eq!(entry.result_type, text(3));
        }
    }
}

#[test]
fn retained_integer_bitwise_operators_preserve_width_and_current_inputs() {
    let control = ProductionControl::uncontrolled();
    for (ty, left, count, expected) in [
        (ColumnType::SmallInteger, 1, 16, 0),
        (ColumnType::SmallInteger, 1, 32, 1),
        (ColumnType::SmallInteger, -7, 32, -7),
        (ColumnType::Integer, 1, 31, -2_147_483_648),
        (ColumnType::Integer, 1, 32, 1),
        (ColumnType::BigInteger, 1, 63, i64::MIN),
        (ColumnType::BigInteger, 1, 64, 1),
    ] {
        let (_, selected) = eval_numeric_operator_with_resolution(
            NumericOperator::ShiftLeft,
            &[Value::Int(left), Value::Int(count)],
            &[Some(ty), Some(ColumnType::Integer)],
            &control,
        )
        .unwrap();
        let selected = selected.expect("fixed integer selection");
        assert_eq!(
            *selected
                .evaluate(&[Value::Int(left), Value::Int(count)], &control)
                .unwrap(),
            Value::Int(expected)
        );
        assert_eq!(
            *selected
                .evaluate(&[Value::Int(7), Value::Int(0)], &control)
                .unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            *selected
                .evaluate(&[Value::Null, Value::Int(0)], &control)
                .unwrap(),
            Value::Null
        );
        assert_eq!(
            selected
                .evaluate(&[Value::Null, Value::Int(i64::MAX)], &control)
                .unwrap_err()
                .sqlstate(),
            Some("22003")
        );
    }
    let (value, selected) = eval_numeric_operator_with_resolution(
        NumericOperator::BitAnd,
        &[Value::Str("7".into()), Value::Int(3)],
        &[None, Some(ColumnType::Integer)],
        &control,
    )
    .unwrap();
    assert_eq!(*value, Value::Int(3));
    assert!(
        selected.is_none(),
        "unknown input must retain conversion on every invocation"
    );
}

#[test]
fn retained_integer_bitwise_operators_use_invoking_allowance_and_cancellation() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let (_, selected) = eval_numeric_operator_with_resolution(
        NumericOperator::BitAnd,
        &[Value::Int(7), Value::Int(3)],
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    let selected = selected.unwrap();
    let budget = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("live owner").unwrap();
    for value in 0..512 {
        let output = selected
            .evaluate(&[Value::Int(value), Value::Int(3)], &control)
            .unwrap();
        assert_eq!(*output, Value::Int(value % 4));
        drop(output);
        assert_eq!(budget.used(), held.reserved_bytes());
    }
    let occupied = budget.reserve(budget.limit() - budget.used()).unwrap();
    assert_eq!(
        selected
            .evaluate(&[Value::Int(7), Value::Int(3)], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            selected
                .evaluate(&[Value::Int(7), Value::Int(3)], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), held.reserved_bytes());
        token.reset();
    }
    drop(held);
    assert_eq!(budget.used(), 0);
}
