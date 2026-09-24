//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DecimalValue};

fn array(elements: Vec<Value>, lower: i32) -> Value {
    Value::Array(ArrayValue::with_lower_bounds(elements, vec![lower]).unwrap())
}

fn evaluate(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Produced<Value> {
    eval_array_functions_with_control(name, args, control)
        .unwrap()
        .unwrap()
}

#[test]
fn controlled_array_outputs_preserve_bounds_nulls_and_owned_payloads() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let source = array(
        vec![
            Value::Str("first".into()),
            Value::Null,
            Value::Str("last".into()),
        ],
        -2,
    );
    for (name, args, expected) in owned_array_cases(&source)
        .into_iter()
        .chain(transformed_array_cases(&source))
    {
        let output = evaluate(name, &args, &control);
        assert_eq!(&*output, &expected, "{name}");
        assert_eq!(budget.used(), output.reserved_bytes());
        if let (Value::Array(input), Value::Array(result)) = (&source, &*output) {
            assert_ne!(input.elements().as_ptr(), result.elements().as_ptr());
        }
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(
        source,
        array(
            vec![
                Value::Str("first".into()),
                Value::Null,
                Value::Str("last".into())
            ],
            -2
        )
    );
}

#[test]
fn controlled_array_order_is_stable_and_preserves_json_comparison_errors() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let numeric = array(
        vec![
            Value::Decimal(DecimalValue::parse("1.00").unwrap()),
            Value::Decimal(DecimalValue::parse("0.2").unwrap()),
            Value::Decimal(DecimalValue::parse("1.0").unwrap()),
            Value::Null,
        ],
        4,
    );
    for (descending, expected) in [
        (false, ["0.2", "1.00", "1.0", "NULL"]),
        (true, ["NULL", "1.00", "1.0", "0.2"]),
    ] {
        let output = evaluate(
            "array_sort",
            &[numeric.clone(), Value::Bool(descending)],
            &control,
        );
        let Value::Array(result) = &*output else {
            panic!("array result");
        };
        for (value, text) in result.elements().iter().zip(expected) {
            match value {
                Value::Decimal(value) => assert_eq!(value.to_sql_string(), text),
                Value::Null => assert_eq!(text, "NULL"),
                _ => panic!("numeric element"),
            }
        }
        assert_eq!(result.lower_bounds(), &[4]);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let json = array(vec![Value::Json("1".into()), Value::Json("2".into())], 1);
    assert_eq!(
        eval_dispatched_json_array_sort_with_control(&[json], &control)
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    let rows = array(
        vec![
            Value::Row(vec![Value::Json("1".into())]),
            Value::Row(vec![Value::Json("2".into())]),
        ],
        1,
    );
    assert_eq!(
        eval_array_functions_with_control("array_sort", &[rows], &control)
            .unwrap()
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_array_concatenation_keeps_multidimensional_shape_and_subscripts() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = array(vec![Value::Int(1), Value::Int(2)], 5);
    let matrix = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![Value::List(vec![Value::Int(3), Value::Int(4)])],
            vec![-1, 5],
        )
        .unwrap(),
    );
    let output = evaluate("array_cat", &[row, matrix], &control);
    let Value::Array(result) = &*output else {
        panic!("array result");
    };
    assert_eq!(result.dimensions(), &[2, 2]);
    assert_eq!(result.lower_bounds(), &[-1, 5]);
    assert_eq!(
        result.elements(),
        &[
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)])
        ]
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn array_quota_and_both_cancellations_release_partial_outputs() {
    let source = array(
        vec![Value::Str("x".repeat(32)), Value::Str("y".repeat(1024))],
        1,
    );
    let budget = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for name in ["array_reverse", "array_sort", "unnest"] {
        let error =
            eval_array_functions_with_control(name, std::slice::from_ref(&source), &control)
                .unwrap()
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        *evaluate("array_length", &[source.clone(), Value::Int(1)], &control),
        Value::Int(2)
    );
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(1 << 20);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = eval_array_functions_with_control(
            "array_reverse",
            std::slice::from_ref(&source),
            &control,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

fn owned_array_cases(source: &Value) -> [(&'static str, Vec<Value>, Value); 4] {
    [
        (
            "array_dims",
            vec![source.clone()],
            Value::Str("[-2:0]".into()),
        ),
        (
            "array_append",
            vec![source.clone(), Value::Str("new".into())],
            array(
                vec![
                    Value::Str("first".into()),
                    Value::Null,
                    Value::Str("last".into()),
                    Value::Str("new".into()),
                ],
                -2,
            ),
        ),
        (
            "array_prepend",
            vec![Value::Str("new".into()), source.clone()],
            array(
                vec![
                    Value::Str("new".into()),
                    Value::Str("first".into()),
                    Value::Null,
                    Value::Str("last".into()),
                ],
                -2,
            ),
        ),
        (
            "array_remove",
            vec![source.clone(), Value::Null],
            array(
                vec![Value::Str("first".into()), Value::Str("last".into())],
                -2,
            ),
        ),
    ]
}

fn transformed_array_cases(source: &Value) -> [(&'static str, Vec<Value>, Value); 4] {
    [
        (
            "array_position",
            vec![source.clone(), Value::Null],
            Value::Int(-1),
        ),
        (
            "array_reverse",
            vec![source.clone()],
            array(
                vec![
                    Value::Str("last".into()),
                    Value::Null,
                    Value::Str("first".into()),
                ],
                -2,
            ),
        ),
        (
            "unnest",
            vec![source.clone()],
            Value::List(vec![
                Value::Str("first".into()),
                Value::Null,
                Value::Str("last".into()),
            ]),
        ),
        (
            "array_cat",
            vec![Value::Null, source.clone()],
            source.clone(),
        ),
    ]
}
