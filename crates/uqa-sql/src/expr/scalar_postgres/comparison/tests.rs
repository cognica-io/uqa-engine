//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};

fn array(elements: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(elements).unwrap())
}

#[test]
fn controlled_quantified_comparison_preserves_nested_null_and_empty_semantics() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let nested = array(vec![
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::List(vec![Value::Int(2), Value::Int(3)]),
    ]);
    for (dispatch, needle, values, expected) in [
        (
            FunctionDispatch::AnyOperator,
            Value::Int(2),
            nested.clone(),
            Value::Bool(true),
        ),
        (
            FunctionDispatch::AnyOperator,
            Value::Int(4),
            nested.clone(),
            Value::Null,
        ),
        (
            FunctionDispatch::AllOperator,
            Value::Int(2),
            nested,
            Value::Bool(false),
        ),
        (
            FunctionDispatch::AnyOperator,
            Value::Null,
            array(vec![]),
            Value::Bool(false),
        ),
        (
            FunctionDispatch::AllOperator,
            Value::Null,
            array(vec![]),
            Value::Bool(true),
        ),
        (
            FunctionDispatch::AllOperator,
            Value::Int(1),
            Value::Null,
            Value::Null,
        ),
    ] {
        let output = evaluate(
            dispatch,
            &[needle, values, Value::Str("=".into())],
            &control,
        )
        .unwrap()
        .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(
            budget.used(),
            0,
            "comparison scratch ends before result handoff"
        );
    }
}

#[test]
fn controlled_comparison_keeps_early_decisions_and_first_typed_errors() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let values = array(vec![Value::Int(2), Value::Bool(true)]);
    for (dispatch, needle, expected) in [
        (
            FunctionDispatch::AnyOperator,
            Value::Int(1),
            Value::Bool(true),
        ),
        (
            FunctionDispatch::AllOperator,
            Value::Int(3),
            Value::Bool(false),
        ),
    ] {
        assert_eq!(
            *evaluate(
                dispatch,
                &[needle, values.clone(), Value::Str("<".into())],
                &control
            )
            .unwrap()
            .unwrap(),
            expected
        );
    }
    for (dispatch, args, expected) in [
        (
            FunctionDispatch::BetweenSymmetric,
            vec![Value::Int(2), Value::Int(3), Value::Int(1)],
            Value::Bool(true),
        ),
        (
            FunctionDispatch::BetweenSymmetric,
            vec![Value::Int(2), Value::Null, Value::Int(1)],
            Value::Null,
        ),
        (
            FunctionDispatch::IsDistinct,
            vec![Value::Null, Value::Null],
            Value::Bool(false),
        ),
        (
            FunctionDispatch::IsDistinct,
            vec![Value::Null, Value::Int(1)],
            Value::Bool(true),
        ),
    ] {
        assert_eq!(
            *evaluate(dispatch, &args, &control).unwrap().unwrap(),
            expected
        );
    }
    assert!(matches!(
        evaluate(
            FunctionDispatch::BetweenSymmetric,
            &[Value::Int(2), Value::Int(3), Value::Bool(true)],
            &control
        )
        .unwrap(),
        Err(SQLError::TypeMismatch(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn quantified_comparison_quota_and_both_cancellation_scopes_release_scratch() {
    let args = [
        Value::Int(1),
        array(vec![Value::Int(2)]),
        Value::Str("=".into()),
    ];
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(4);
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        evaluate(FunctionDispatch::AnyOperator, &args, &control)
            .unwrap()
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let held = control.copy_text("earlier result").unwrap();
        let used = budget.used();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            evaluate(FunctionDispatch::AllOperator, &args, &control)
                .unwrap()
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), used);
        assert_eq!(&*held, "earlier result");
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}
