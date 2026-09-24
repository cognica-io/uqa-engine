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
    eval_postgres_arrays_with_control(name, args, control)
        .unwrap()
        .unwrap()
}

#[test]
fn array_transforms_preserve_null_matching_bounds_and_owned_results() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let source = array(
        vec![
            Value::Str("one".into()),
            Value::Null,
            Value::Str("three".into()),
        ],
        -2,
    );
    for (name, args, expected) in [
        (
            "array_positions",
            vec![source.clone(), Value::Null],
            array(vec![Value::Int(-1)], 1),
        ),
        (
            "array_replace",
            vec![source.clone(), Value::Null, Value::Str("two".into())],
            array(
                vec![
                    Value::Str("one".into()),
                    Value::Str("two".into()),
                    Value::Str("three".into()),
                ],
                -2,
            ),
        ),
        (
            "array_to_string",
            vec![
                source.clone(),
                Value::Str("/".into()),
                Value::Str("nil".into()),
            ],
            Value::Str("one/nil/three".into()),
        ),
        (
            "trim_array",
            vec![source.clone(), Value::Int(1)],
            array(vec![Value::Str("one".into()), Value::Null], 1),
        ),
    ] {
        let output = evaluate(name, &args, &control);
        assert_eq!(*output, expected, "{name}");
        assert_eq!(budget.used(), output.reserved_bytes());
        assert!(output.reserved_bytes() > 0);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(
        source,
        array(
            vec![
                Value::Str("one".into()),
                Value::Null,
                Value::Str("three".into())
            ],
            -2
        )
    );
}

#[test]
fn multidimensional_fill_and_flattened_predicates_share_controlled_native_traversal() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let matrix = evaluate(
        "array_fill",
        &[
            Value::Str("x".into()),
            array(vec![Value::Int(2), Value::Int(3)], 1),
            array(vec![Value::Int(-1), Value::Int(5)], 1),
        ],
        &control,
    );
    let Value::Array(value) = &*matrix else {
        panic!("array result");
    };
    assert_eq!(value.dimensions(), &[2, 3]);
    assert_eq!(value.lower_bounds(), &[-1, 5]);
    let retained = budget.used();
    assert_eq!(retained, matrix.reserved_bytes());
    for (name, args, expected) in [
        (
            "array_to_string",
            vec![(*matrix).clone(), Value::Str("".into())],
            Value::Str("xxxxxx".into()),
        ),
        (
            "array_overlap",
            vec![
                (*matrix).clone(),
                array(vec![Value::Null, Value::Str("x".into())], 1),
            ],
            Value::Bool(true),
        ),
        (
            "contains_op",
            vec![(*matrix).clone(), array(vec![Value::Str("x".into())], 1)],
            Value::Bool(true),
        ),
        (
            "contained_by_op",
            vec![array(vec![Value::Null], 1), (*matrix).clone()],
            Value::Bool(false),
        ),
        (
            "contains_op",
            vec![
                Value::JsonB("{\"a\":1,\"b\":2}".into()),
                Value::JsonB("{\"a\":1}".into()),
            ],
            Value::Bool(true),
        ),
    ] {
        let output = evaluate(name, &args, &control);
        assert_eq!(*output, expected, "{name}");
        assert_eq!(budget.used(), retained + output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), retained);
    }
    drop(matrix);
    assert_eq!(budget.used(), 0);
}

#[test]
fn array_producer_quota_and_cancellation_release_partial_copies_and_comparisons() {
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let source = array(vec![Value::Str("x".repeat(1024))], 1);
    for (name, args) in [
        (
            "array_replace",
            vec![source.clone(), Value::Null, Value::Null],
        ),
        (
            "array_to_string",
            vec![source.clone(), Value::Str("".into())],
        ),
        (
            "array_fill",
            vec![Value::Str("y".repeat(1024)), array(vec![Value::Int(2)], 1)],
        ),
        ("trim_array", vec![source, Value::Int(0)]),
    ] {
        let error = eval_postgres_arrays_with_control(name, &args, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
    let decimals = array(
        vec![Value::Decimal(DecimalValue::parse("1.00").unwrap())],
        1,
    );
    let other = array(vec![Value::Decimal(DecimalValue::parse("1.0").unwrap())], 1);
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = eval_postgres_arrays_with_control(
            "array_overlap",
            &[decimals.clone(), other.clone()],
            &control,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
