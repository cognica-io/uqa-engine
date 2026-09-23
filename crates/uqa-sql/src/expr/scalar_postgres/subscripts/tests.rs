//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn array(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Value {
    Value::Array(ArrayValue::with_lower_bounds(elements, lower_bounds).unwrap())
}

fn matrix() -> Value {
    array(
        vec![
            Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]),
            Value::List(vec![Value::Str("c".into()), Value::Str("d".into())]),
        ],
        vec![-1, 5],
    )
}

#[test]
fn subscripts_preserve_multidimensional_bounds_and_selected_owned_values() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (dispatch, args, expected) in [
        (
            FunctionDispatch::ArraySubscripts,
            vec![matrix(), Value::Int(0), Value::Int(6)],
            Value::Str("d".into()),
        ),
        (
            FunctionDispatch::Subscript,
            vec![matrix(), Value::Int(-1)],
            array(
                vec![Value::Str("a".into()), Value::Str("b".into())],
                vec![5],
            ),
        ),
        (
            FunctionDispatch::ArraySubscripts,
            vec![matrix(), Value::Int(-1)],
            Value::Null,
        ),
        (
            FunctionDispatch::Subscript,
            vec![
                Value::Map([(String::from("7"), Value::Str("seven".into()))].into()),
                Value::Int(7),
            ],
            Value::Str("seven".into()),
        ),
    ] {
        let output = eval_postgres_subscript_with_control(dispatch, &args, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        assert_eq!(
            super::super::eval_dispatched_postgres_function(dispatch, &args)
                .unwrap()
                .unwrap(),
            expected
        );
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn slices_clamp_each_dimension_and_normalize_result_bounds() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (dispatch, args, expected) in [
        (
            FunctionDispatch::ArraySlices,
            vec![
                matrix(),
                Value::Int(-100),
                Value::Int(-1),
                Value::Int(6),
                Value::Null,
            ],
            array(vec![Value::List(vec![Value::Str("b".into())])], vec![1, 1]),
        ),
        (
            FunctionDispatch::ArraySlices,
            vec![matrix(), Value::Int(0), Value::Null],
            array(
                vec![Value::List(vec![
                    Value::Str("c".into()),
                    Value::Str("d".into()),
                ])],
                vec![1, 1],
            ),
        ),
        (
            FunctionDispatch::Slice,
            vec![matrix(), Value::Int(0), Value::Int(100)],
            array(
                vec![Value::List(vec![
                    Value::Str("c".into()),
                    Value::Str("d".into()),
                ])],
                vec![1, 1],
            ),
        ),
        (
            FunctionDispatch::Slice,
            vec![matrix(), Value::Int(100), Value::Null],
            Value::Array(ArrayValue::try_new(Vec::new()).unwrap()),
        ),
    ] {
        let output = eval_postgres_subscript_with_control(dispatch, &args, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn failed_subscript_copies_and_cancelled_slice_readers_release_their_allowance() {
    let budget = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let source = array(vec![Value::Str("x".repeat(4096))], vec![1]);
    let control = ProductionControl::new(&budget, &token, &token);
    for (dispatch, indices) in [
        (FunctionDispatch::Subscript, vec![Value::Int(1)]),
        (FunctionDispatch::ArraySubscripts, vec![Value::Int(1)]),
        (FunctionDispatch::Slice, vec![Value::Null, Value::Null]),
        (
            FunctionDispatch::ArraySlices,
            vec![Value::Null, Value::Null],
        ),
    ] {
        let mut args = vec![source.clone()];
        args.extend(indices);
        let error = eval_postgres_subscript_with_control(dispatch, &args, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
        for original_cancelled in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            let error = eval_postgres_subscript_with_control(dispatch, &args, &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
}
